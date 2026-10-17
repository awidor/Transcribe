#include "azure/internal.hpp"
#include "support/server.hpp"
#include "transcribe/azure.hpp"

#include <gtest/gtest.h>

#include <condition_variable>
#include <functional>
#include <future>
#include <mutex>
#include <optional>
#include <thread>

using namespace transcribe;
using namespace std::chrono_literals;

namespace {

std::uint8_t byte(char c) {
    return static_cast<std::uint8_t>(c);
}

// Test-only SHA-1, for the WebSocket handshake.
std::string sha1(std::string data) {
    std::uint64_t bits = data.size() * 8;
    data += '\x80';
    while (data.size() % 64 != 56)
        data += '\0';
    for (int shift = 56; shift >= 0; shift -= 8)
        data += static_cast<char>(bits >> shift);
    auto rotate = [](std::uint32_t x, int n) { return x << n | x >> (32 - n); };
    std::uint32_t h[5] = {0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0};
    for (std::size_t block = 0; block < data.size(); block += 64) {
        std::uint32_t w[80];
        for (int i = 0; i < 16; ++i) {
            const char* p = &data[block + 4 * i];
            w[i] = byte(p[0]) << 24 | byte(p[1]) << 16 | byte(p[2]) << 8 | byte(p[3]);
        }
        for (int i = 16; i < 80; ++i)
            w[i] = rotate(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
        auto [a, b, c, d, e] = h;
        for (int i = 0; i < 80; ++i) {
            std::uint32_t f = b ^ c ^ d, k = i < 40 ? 0x6ED9EBA1 : 0xCA62C1D6;
            if (i < 20) {
                f = (b & c) | (~b & d);
                k = 0x5A827999;
            } else if (i >= 40 && i < 60) {
                f = (b & c) | (b & d) | (c & d);
                k = 0x8F1BBCDC;
            }
            auto t = rotate(a, 5) + f + e + k + w[i];
            e = d;
            d = c;
            c = rotate(b, 30);
            b = a;
            a = t;
        }
        h[0] += a;
        h[1] += b;
        h[2] += c;
        h[3] += d;
        h[4] += e;
    }
    std::string digest;
    for (auto v : h)
        for (int shift = 24; shift >= 0; shift -= 8)
            digest += static_cast<char>(v >> shift);
    return digest;
}

std::string base64(std::string_view bytes) {
    static constexpr char digits[] =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::string out;
    for (std::size_t i = 0; i < bytes.size(); i += 3) {
        std::uint32_t n = byte(bytes[i]) << 16;
        if (i + 1 < bytes.size()) n |= byte(bytes[i + 1]) << 8;
        if (i + 2 < bytes.size()) n |= byte(bytes[i + 2]);
        out += digits[n >> 18 & 63];
        out += digits[n >> 12 & 63];
        out += i + 1 < bytes.size() ? digits[n >> 6 & 63] : '=';
        out += i + 2 < bytes.size() ? digits[n & 63] : '=';
    }
    return out;
}

enum Opcode { TEXT = 1, BINARY = 2, CLOSE = 8 };
using Received = std::pair<std::string, std::string>;

// The server side of a WebSocket, enough to stand in for Azure.
class WebSocket {
  public:
    explicit WebSocket(test::Connection connection)
        : connection_(std::move(connection)), request_(connection_.read_request()) {
        auto key = request_.header("Sec-WebSocket-Key") + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
        connection_.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                          "Connection: Upgrade\r\nSec-WebSocket-Accept: " +
                          base64(sha1(key)) + "\r\n\r\n");
    }
    const test::HttpRequest& request() const { return request_; }

    // The next client message as its opcode and unmasked payload; opcode 0 once it is gone.
    std::pair<int, std::string> read() {
        int opcode = 0;
        std::string message;
        for (;;) {
            auto head = connection_.read_exact(2);
            if (head.size() < 2) return {};
            std::uint64_t length = byte(head[1]) & 0x7f;
            if (length >= 126) {
                auto extended = connection_.read_exact(length == 126 ? 2 : 8);
                length = 0;
                for (char c : extended)
                    length = length << 8 | byte(c);
            }
            auto mask = byte(head[1]) & 0x80 ? connection_.read_exact(4) : std::string();
            auto payload = connection_.read_exact(length);
            if (payload.size() < length || (!mask.empty() && mask.size() < 4)) return {};
            for (std::size_t i = 0; i < payload.size() && !mask.empty(); ++i)
                payload[i] ^= mask[i % 4];
            if (byte(head[0]) & 0x0f) opcode = byte(head[0]) & 0x0f;
            message += payload;
            if (byte(head[0]) & 0x80) return {opcode, message};
        }
    }

    void send(int opcode, std::string_view payload) {
        std::string frame(1, static_cast<char>(0x80 | opcode));
        if (payload.size() < 126) {
            frame += static_cast<char>(payload.size());
        } else if (payload.size() < 65536) {
            frame += static_cast<char>(126);
            frame += static_cast<char>(payload.size() >> 8);
            frame += static_cast<char>(payload.size() & 0xff);
        } else {
            frame += static_cast<char>(127);
            for (int shift = 56; shift >= 0; shift -= 8)
                frame += static_cast<char>(static_cast<std::uint64_t>(payload.size()) >> shift);
        }
        frame += payload;
        connection_.write(frame);
    }

  private:
    test::Connection connection_;
    test::HttpRequest request_;
};

// Accepts one client on a thread, checks the key it upgrades with, then runs `script`.
class FakeAzure {
  public:
    explicit FakeAzure(std::function<void(WebSocket&)> script)
        : thread_([this, script = std::move(script)] {
              try {
                  WebSocket ws(server_.accept());
                  EXPECT_EQ(ws.request().header("Ocp-Apim-Subscription-Key"), "test-key");
                  script(ws);
              } catch (const std::exception& error) {
                  ADD_FAILURE() << error.what();
              }
          }) {}
    std::string url() const { return "ws://127.0.0.1:" + std::to_string(server_.port()); }

  private:
    test::Server server_;
    std::jthread thread_;
};

// Reads a client frame as its path and body; audio bodies are returned raw.
Received receive(WebSocket& ws) {
    auto [opcode, payload] = ws.read();
    if (opcode == TEXT) {
        auto [path, body] = azure::parse(payload);
        return {path, body.dump()};
    }
    if (opcode == BINARY && payload.size() >= 2) {
        auto length = std::size_t{byte(payload[0])} << 8 | byte(payload[1]);
        EXPECT_TRUE(payload.substr(2, length).starts_with("Path: audio\r\n"));
        return {"audio", payload.substr(2 + length)};
    }
    ADD_FAILURE() << "Unexpected opcode " << opcode;
    return {};
}

std::string reply(const std::string& path, const std::string& body) {
    return "X-RequestId: r\r\nContent-Type: application/json; charset=utf-8\r\nPath: " + path +
           "\r\n\r\n" + body;
}

void setup(WebSocket& ws) {
    EXPECT_EQ(receive(ws).first, "speech.config");
    auto [path, context] = receive(ws);
    EXPECT_EQ(path, "speech.context");
    EXPECT_EQ(nlohmann::json::parse(context)["model"]["name"], azure::MODEL);
    auto [audio, header] = receive(ws);
    ASSERT_EQ(audio, "audio");
    ASSERT_EQ(header.size(), 44u);
    EXPECT_EQ(header.substr(0, 4), "RIFF");
    EXPECT_EQ(byte(header[24]) | byte(header[25]) << 8 | byte(header[26]) << 16 |
                  byte(header[27]) << 24,
              24'000);
}

void ignore(const azure::Words&) {}

} // namespace

TEST(Azure, AKeyFindsTheOneRegionThatAcceptsIt) {
    test::Server server;
    std::jthread serving([&](std::stop_token stop) {
        while (!stop.stop_requested()) {
            try {
                auto connection = server.accept(100ms);
                auto request = connection.read_request();
                EXPECT_EQ(request.header("Ocp-Apim-Subscription-Key"), "test-key");
                bool home = request.method == "POST" && request.target == "/home";
                connection.respond(home ? 200 : 401, "");
            } catch (const std::runtime_error&) {
                // No connection yet, or an abandoned probe.
            }
        }
    });
    auto probe = [&](std::string region) { return std::pair{region, server.url() + "/" + region}; };
    std::pair<std::string, std::string> offline{"offline", "http://127.0.0.1:1/"};
    EXPECT_EQ(azure::find_region("test-key", {probe("away"), probe("home"), offline}), "home");
    try {
        azure::find_region("test-key", {probe("away")});
        ADD_FAILURE();
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Azure API key rejected");
    }
    try {
        azure::find_region("test-key", {offline});
        ADD_FAILURE();
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Could not connect to Azure");
    }
}

TEST(Azure, StreamsAudioShowsWordsAndReturnsEveryPhrase) {
    FakeAzure azure([](WebSocket& ws) {
        setup(ws);
        EXPECT_EQ(receive(ws), Received("audio", std::string(9600, '\0')));
        for (const auto& message :
             {reply("turn.start", "{}"), reply("speech.hypothesis", R"({"Text":"Hello"})"),
              reply("speech.phrase",
                    R"({"RecognitionStatus":"Success","DisplayText":"Hello world."})"),
              reply("speech.hypothesis", R"({"Text":"How are"})")})
            ws.send(TEXT, message);
        EXPECT_EQ(receive(ws), Received("audio", ""));
        for (const auto& message :
             {reply("speech.phrase",
                    R"({"RecognitionStatus":"Success","DisplayText":"How are you?"})"),
              reply("speech.phrase", R"({"RecognitionStatus":"EndOfDictation"})"),
              reply("turn.end", "{}")})
            ws.send(TEXT, message);
    });
    auto stream = azure::connect_to(azure.url(), "test-key", {});
    auto audio = std::make_shared<Channel<audio::Packet>>(4);
    std::mutex mutex;
    std::condition_variable changed;
    azure::Words shown;
    auto running = std::async(std::launch::async, [&] {
        return stream.run(audio,
                          [&](const azure::Words& words) {
                              std::lock_guard lock(mutex);
                              shown = words;
                              changed.notify_all();
                          },
                          {});
    });
    audio->send({std::vector<std::uint8_t>(9600, 0), std::nullopt});
    {
        std::unique_lock lock(mutex);
        EXPECT_TRUE(changed.wait_for(lock, 10s, [&] { return shown.interim == " How are"; }));
        EXPECT_EQ(shown.text, "Hello world.");
    }
    audio->close();
    auto [text, seconds] = running.get();
    EXPECT_EQ(text, "Hello world. How are you?");
    EXPECT_EQ(seconds, 0.2);
}

TEST(Azure, ShortAudioIsSilenceAndRejectionsAreReported) {
    {
        FakeAzure azure([](WebSocket& ws) {
            setup(ws);
            ws.read();
        });
        auto stream = azure::connect_to(azure.url(), "test-key", {});
        auto audio = std::make_shared<Channel<audio::Packet>>(1);
        audio->send({std::vector<std::uint8_t>(100, 0), std::nullopt});
        audio->close();
        EXPECT_THROW(stream.run(audio, ignore, {}), audio::NoSpeech);
    }
    FakeAzure azure([](WebSocket& ws) {
        setup(ws);
        // Close code 1007, as tungstenite's CloseCode::Invalid.
        ws.send(CLOSE, std::string("\x03\xef") + " Unsupported model. ");
    });
    auto stream = azure::connect_to(azure.url(), "test-key", {});
    auto audio = std::make_shared<Channel<audio::Packet>>(1);
    try {
        stream.run(audio, ignore, {});
        ADD_FAILURE();
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Azure: Unsupported model.");
    }
}

TEST(Azure, ADroppedConnectionKeepsTheConfirmedWords) {
    FakeAzure azure([](WebSocket& ws) {
        setup(ws);
        ws.send(TEXT, reply("speech.phrase",
                            R"({"RecognitionStatus":"Success","DisplayText":"Kept words."})"));
    });
    auto stream = azure::connect_to(azure.url(), "test-key", {});
    auto audio = std::make_shared<Channel<audio::Packet>>(1);
    azure::Words kept;
    EXPECT_THROW(stream.run(audio, [&](const azure::Words& words) { kept = words; }, {}), Error);
    EXPECT_EQ(kept.text, "Kept words.");
}

TEST(Azure, CancelInterruptsAStalledConnection) {
    test::Server server;
    std::stop_source cancel;
    std::jthread canceller([&] {
        std::this_thread::sleep_for(20ms);
        cancel.request_stop();
    });
    auto connecting = std::async(std::launch::async, [&] {
        return azure::connect_to("ws://127.0.0.1:" + std::to_string(server.port()), "secret",
                                 cancel.get_token());
    });
    ASSERT_EQ(connecting.wait_for(1s), std::future_status::ready);
    try {
        connecting.get();
        ADD_FAILURE();
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Cancelled");
    }
}

TEST(Azure, ARefusedUpgradeIsReported) {
    test::Server server;
    std::jthread refusing([&] {
        auto connection = server.accept();
        connection.read_request();
        connection.respond(401, "");
    });
    try {
        azure::connect_to("ws://127.0.0.1:" + std::to_string(server.port()), "test-key", {});
        ADD_FAILURE();
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Azure API key rejected");
    }
}
