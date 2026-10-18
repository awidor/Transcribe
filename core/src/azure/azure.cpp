#include "transcribe/azure.hpp"

#include "azure/internal.hpp"
#include "transcribe/http.hpp"

#include <curl/curl.h>

#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <memory>
#include <mutex>
#include <optional>
#include <thread>

#ifndef _WIN32
#include <poll.h>
#endif

namespace transcribe::azure {

using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;
using nlohmann::json;

namespace {

// A fifth of a second of audio; anything shorter is treated as silence.
constexpr std::size_t MIN_BYTES = audio::SAMPLE_RATE * 2 / 5;
// Packets are batched up to a tenth of a second per message.
constexpr std::size_t CHUNK_BYTES = audio::SAMPLE_RATE * 2 / 10;
constexpr std::size_t MAX_MESSAGE_BYTES = 1024 * 1024;

// shortcut: a fixed list of public Azure regions; add new ones as Azure opens them.
constexpr const char* REGIONS[] = {
    "australiaeast",
    "australiasoutheast",
    "brazilsouth",
    "canadacentral",
    "canadaeast",
    "centralindia",
    "centralus",
    "chilecentral",
    "eastasia",
    "eastus",
    "eastus2",
    "francecentral",
    "germanywestcentral",
    "indonesiacentral",
    "israelcentral",
    "italynorth",
    "japaneast",
    "japanwest",
    "jioindiawest",
    "koreacentral",
    "malaysiawest",
    "mexicocentral",
    "newzealandnorth",
    "northcentralus",
    "northeurope",
    "norwayeast",
    "polandcentral",
    "qatarcentral",
    "southafricanorth",
    "southcentralus",
    "southeastasia",
    "southindia",
    "spaincentral",
    "swedencentral",
    "switzerlandnorth",
    "switzerlandwest",
    "uaenorth",
    "uksouth",
    "ukwest",
    "westcentralus",
    "westeurope",
    "westus",
    "westus2",
    "westus3",
};

// Control characters in a header value would smuggle in more headers.
bool valid_header(std::string_view value) {
    return std::none_of(value.begin(), value.end(),
                        [](unsigned char c) { return (c < 32 && c != '\t') || c == 127; });
}

struct Message {
    unsigned flags;
    std::string bytes;
};

Message text_message(const std::string& path, const std::string& request, const json& body) {
    return {CURLWS_TEXT, "Path: " + path + "\r\nX-RequestId: " + request +
                             "\r\nContent-Type: application/json\r\n\r\n" + body.dump()};
}

// Audio frames carry their headers behind a two-byte length.
Message audio_message(const std::string& request, const std::vector<std::uint8_t>& body,
                      bool content_type) {
    auto headers = "Path: audio\r\nX-RequestId: " + request + "\r\n" +
                   (content_type ? "Content-Type: audio/x-wav\r\n" : "");
    std::string bytes{static_cast<char>(headers.size() >> 8),
                      static_cast<char>(headers.size() & 0xff)};
    bytes += headers;
    bytes.append(body.begin(), body.end());
    return {CURLWS_BINARY, std::move(bytes)};
}

// A WAV header announcing `data` bytes of mono 16-bit PCM at SAMPLE_RATE; a stream announces
// no length.
std::vector<std::uint8_t> wave_header(std::uint32_t data = 0) {
    std::vector<std::uint8_t> header;
    auto text = [&](std::string_view s) { header.insert(header.end(), s.begin(), s.end()); };
    auto number = [&](std::uint32_t value, int size) {
        for (int i = 0; i < size; ++i)
            header.push_back(static_cast<std::uint8_t>(value >> 8 * i));
    };
    text("RIFF");
    number(data ? 36 + data : 0, 4);
    text("WAVEfmt ");
    number(16, 4);
    number(1, 2);
    number(1, 2);
    number(audio::SAMPLE_RATE, 4);
    number(audio::SAMPLE_RATE * 2, 4);
    number(2, 2);
    number(16, 2);
    text("data");
    number(data, 4);
    return header;
}

std::string field(const json& body, const char* name) {
    if (!body.is_object()) return {};
    auto value = body.find(name);
    return value != body.end() && value->is_string() ? value->get<std::string>() : std::string();
}

// Waits until the socket has data, or room when `writable`, for at most `timeout`.
void wait(curl_socket_t socket, bool writable, std::chrono::milliseconds timeout) {
    auto events = static_cast<short>(POLLIN | (writable ? POLLOUT : 0));
#ifdef _WIN32
    WSAPOLLFD fd{socket, events, 0};
    WSAPoll(&fd, 1, static_cast<int>(timeout.count()));
#else
    pollfd fd{socket, events, 0};
    ::poll(&fd, 1, static_cast<int>(timeout.count()));
#endif
}

} // namespace

std::string region(const std::string& key) {
    std::vector<std::pair<std::string, std::string>> probes;
    for (const char* region : REGIONS)
        probes.emplace_back(region, std::string("https://") + region +
                                        ".api.cognitive.microsoft.com/sts/v1.0/issueToken");
    return find_region(key, probes);
}

std::string find_region(const std::string& key,
                        const std::vector<std::pair<std::string, std::string>>& probes) {
    auto value = trim(key);
    // Such a key cannot be sent, so no probe would reach Azure.
    if (!valid_header(value)) throw Error("Could not connect to Azure");
    // Shared with the probes, which outlive this call once one region answers.
    struct Search {
        std::mutex mutex;
        std::condition_variable answered;
        std::optional<std::string> found;
        bool reached = false;
        std::size_t finished = 0;
        std::stop_source done;
    };
    auto search = std::make_shared<Search>();
    for (const auto& [region, url] : probes)
        std::thread([search, value, region, url] {
            std::optional<long> status;
            try {
                // Only the status matters, so reading stops at the body.
                status = http::perform(
                    {.method = "POST",
                     .url = url,
                     .headers = {"Ocp-Apim-Subscription-Key: " + value},
                     .body = "",
                     .timeout = 10s},
                    [](long, std::string_view) { return false; }, search->done.get_token());
            } catch (const http::Failure&) {
            }
            std::lock_guard lock(search->mutex);
            if (status && *status >= 200 && *status < 300) {
                if (!search->found) search->found = region;
            } else if (status) {
                search->reached = true;
            }
            ++search->finished;
            search->answered.notify_all();
        }).detach();
    std::unique_lock lock(search->mutex);
    search->answered.wait(lock, [&] { return search->found || search->finished == probes.size(); });
    // The slower probes are abandoned, as dropping their futures does in Rust.
    search->done.request_stop();
    if (search->found) return *search->found;
    if (search->reached) throw Error("Azure API key rejected");
    throw Error("Could not connect to Azure");
}

std::pair<std::string, json> parse(std::string_view text) {
    auto split = text.find("\r\n\r\n");
    auto headers = text.substr(0, split);
    auto body = split == std::string_view::npos ? std::string_view() : text.substr(split + 4);
    std::optional<std::string> path;
    while (!path && !headers.empty()) {
        auto end = headers.find('\n');
        auto line = headers.substr(0, end);
        headers = end == std::string_view::npos ? std::string_view() : headers.substr(end + 1);
        auto colon = line.find(':');
        if (colon != std::string_view::npos &&
            to_lower_ascii(trim(line.substr(0, colon))) == "path")
            path = to_lower_ascii(trim(line.substr(colon + 1)));
    }
    if (!path) throw Error("Invalid Azure response");
    json value;
    if (!trim(body).empty()) {
        value = json::parse(body, nullptr, false);
        if (value.is_discarded()) throw Error("Invalid Azure response");
    }
    return {*path, value};
}

struct Stream::Socket {
    CURLM* multi = curl_multi_init();
    CURL* curl = curl_easy_init();
    curl_slist* headers = nullptr;
    ~Socket() {
        // Removing the handle closes its connection.
        curl_multi_remove_handle(multi, curl);
        curl_easy_cleanup(curl);
        curl_multi_cleanup(multi);
        curl_slist_free_all(headers);
    }
    // Nested in Stream, so it may build one.
    static Stream open(std::unique_ptr<Socket> socket) { return Stream(std::move(socket)); }
};

Stream::Stream(std::unique_ptr<Socket> socket) : socket_(std::move(socket)) {}
Stream::Stream(Stream&&) noexcept = default;
Stream& Stream::operator=(Stream&&) noexcept = default;
Stream::~Stream() = default;

Stream Stream::connect(const Config& config, std::stop_token stop) {
    return connect_to("wss://" + config.region +
                          ".stt.speech.microsoft.com/stt/speech/universal/v2?format=simple",
                      config.key, stop);
}

Stream connect_to(const std::string& url, const std::string& key, std::stop_token stop) {
    if (stop.stop_requested()) throw Error("Cancelled");
    auto value = trim(key);
    if (!valid_header(value)) throw Error("Invalid Azure API key");
    static const auto global = curl_global_init(CURL_GLOBAL_DEFAULT);
    (void)global;
    auto socket = std::make_unique<Stream::Socket>();
    for (const auto& header :
         {"Ocp-Apim-Subscription-Key: " + value, "X-ConnectionId: " + uuid_simple()}) {
        auto* next = curl_slist_append(socket->headers, header.c_str());
        if (!next) throw Error("Could not connect to Azure");
        socket->headers = next;
    }
    auto* curl = socket->curl;
    if (!socket->multi || !curl) throw Error("Could not connect to Azure");
    curl_easy_setopt(curl, CURLOPT_URL, url.c_str());
    curl_easy_setopt(curl, CURLOPT_CONNECT_ONLY, 2L);
    curl_easy_setopt(curl, CURLOPT_HTTPHEADER, socket->headers);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);
#ifdef _WIN32
    // Schannel fails when a revocation server is unreachable, as behind many proxies.
    curl_easy_setopt(curl, CURLOPT_SSL_OPTIONS, static_cast<long>(CURLSSLOPT_REVOKE_BEST_EFFORT));
#endif
    // The multi interface lets a stop request wake the handshake at once.
    curl_multi_add_handle(socket->multi, curl);
    std::stop_callback wake(stop, [multi = socket->multi] { curl_multi_wakeup(multi); });
    auto deadline = Clock::now() + 20s;
    for (;;) {
        int running = 0;
        curl_multi_perform(socket->multi, &running);
        if (stop.stop_requested()) throw Error("Cancelled");
        int queued = 0;
        auto* done = curl_multi_info_read(socket->multi, &queued);
        if (done && done->msg == CURLMSG_DONE) {
            if (done->data.result == CURLE_OK) break;
            long status = 0;
            curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &status);
            if (status == 401 || status == 403) throw Error("Azure API key rejected");
            if (status == 429) throw Error("Azure rate limit reached");
            // Only a refused upgrade carries a status; anything else never reached Azure.
            if (status != 0 && status != 101)
                throw Error("Azure connection failed (" + std::to_string(status) + ")");
            throw Error("Could not connect to Azure");
        }
        auto left = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - Clock::now());
        if (left.count() <= 0) throw Error("Azure connection timed out");
        curl_multi_poll(socket->multi, nullptr, 0, static_cast<int>(left.count()), nullptr);
    }
    return Stream::Socket::open(std::move(socket));
}

std::pair<std::string, double> Stream::run(audio::Receiver audio,
                                           const std::function<void(const Words&)>& words,
                                           std::stop_token stop) {
    // Rust drops the receiver when the run ends, which stops whatever feeds it.
    struct Close {
        audio::Receiver& audio;
        ~Close() { audio->close(); }
    } close{audio};
    auto* curl = socket_->curl;
    curl_socket_t socket = CURL_SOCKET_BAD;
    curl_easy_getinfo(curl, CURLINFO_ACTIVESOCKET, &socket);
    auto request = uuid_simple();
    std::deque<Message> outgoing{
        text_message(
            "speech.config", request,
            {{"context", {{"system", {{"name", "Transcribe"}, {"version", TRANSCRIBE_VERSION}}}}}}),
        text_message("speech.context", request,
                     {{"phraseDetection", {{"mode", "Conversation"}}},
                      {"phraseOutput", json::object()},
                      {"model", {{"name", MODEL}}}}),
        audio_message(request, wave_header(), true),
    };
    std::size_t offset = 0;
    std::optional<Clock::time_point> stalled;
    std::size_t sent = 0;
    bool ending = false;
    std::optional<Clock::time_point> drained;
    Words current;
    std::string incoming;
    std::string closing;
    std::vector<char> buffer(64 * 1024);
    // curl handles are not thread-safe, so this one thread drives both directions: each pass
    // sends what the socket takes, then handles what has arrived, in the order Rust polls them.
    for (;;) {
        if (stop.stop_requested()) throw Error("Cancelled");
        while (!drained) {
            if (!outgoing.empty()) {
                auto& message = outgoing.front();
                if (!stalled) stalled = Clock::now() + 10s;
                std::size_t written = 0;
                auto result =
                    curl_ws_send(curl, message.bytes.data() + offset, message.bytes.size() - offset,
                                 &written, 0, message.flags);
                offset += written;
                if (result == CURLE_AGAIN) {
                    if (Clock::now() >= *stalled) throw Error("Azure upload stalled");
                    break;
                }
                if (result != CURLE_OK) throw Error("Azure connection lost");
                if (offset < message.bytes.size()) continue;
                outgoing.pop_front();
                offset = 0;
                stalled.reset();
            } else if (ending) {
                drained = Clock::now() + 30s;
            } else {
                // Read first: once closed, nothing can arrive after the try_recv below.
                bool closed = audio->closed();
                auto packet = audio->try_recv();
                if (!packet) {
                    if (!closed) break;
                    if (sent < MIN_BYTES) throw audio::NoSpeech();
                    // An empty audio frame ends the stream; Azure then finishes the last phrase.
                    outgoing.push_back(audio_message(request, {}, false));
                    ending = true;
                    continue;
                }
                if (packet->error) throw Error(*packet->error);
                auto bytes = std::move(packet->bytes);
                while (bytes.size() < CHUNK_BYTES) {
                    auto next = audio->try_recv();
                    if (!next) break;
                    if (next->error) throw Error(*next->error);
                    bytes.insert(bytes.end(), next->bytes.begin(), next->bytes.end());
                }
                if (bytes.empty()) continue;
                sent += bytes.size();
                outgoing.push_back(audio_message(request, bytes, false));
            }
        }
        if (drained && Clock::now() >= *drained)
            throw Error("Azure did not finish the transcript in time");
        for (;;) {
            std::size_t size = 0;
            const curl_ws_frame* meta = nullptr;
            auto result = curl_ws_recv(curl, buffer.data(), buffer.size(), &size, &meta);
            if (result == CURLE_AGAIN) break;
            if (result != CURLE_OK) throw Error("Azure connection lost");
            auto left = static_cast<std::size_t>(meta->bytesleft);
            if (meta->flags & CURLWS_CLOSE) {
                closing.append(buffer.data(), size);
                if (left > 0) continue;
                // The reason follows a two-byte close code.
                auto reason = trim(
                    std::string_view(closing).substr(std::min<std::size_t>(2, closing.size())));
                if (!reason.empty()) throw Error("Azure: " + reason);
                throw Error("Azure closed the connection");
            }
            // curl answers pings itself, and binary messages carry nothing to read.
            if (!(meta->flags & CURLWS_TEXT)) continue;
            if (incoming.size() + size + left > MAX_MESSAGE_BYTES)
                throw Error("Azure connection lost");
            incoming.append(buffer.data(), size);
            if (left > 0 || (meta->flags & CURLWS_CONT)) continue;
            auto [path, body] = parse(std::exchange(incoming, {}));
            if (path == "speech.hypothesis") {
                auto guess = field(body, "Text");
                auto interim = current.text.empty() ? guess : " " + guess;
                if (current.interim != interim) {
                    current.interim = std::move(interim);
                    words(current);
                }
            } else if (path == "speech.phrase") {
                auto status = field(body, "RecognitionStatus");
                if (status == "Success") {
                    auto phrase = trim(field(body, "DisplayText"));
                    if (!phrase.empty()) {
                        if (!current.text.empty()) current.text += ' ';
                        current.text += phrase;
                    }
                    current.interim.clear();
                    words(current);
                } else if (status == "Error") {
                    throw Error("Azure transcription failed");
                }
                // Anything else is silence, or the end of the stream.
            } else if (path == "turn.end") {
                if (!drained) throw Error("Azure stopped transcribing");
                auto text = trim(current.text);
                if (text.empty()) throw audio::NoSpeech();
                return {text, static_cast<double>(sent) / (audio::SAMPLE_RATE * 2)};
            }
        }
        wait(socket, !outgoing.empty(), 10ms);
    }
}

std::pair<std::string, double> transcribe(const Config& config, audio::Receiver audio, bool clean,
                                          std::stop_token stop) {
    return transcribe_at("https://" + config.region +
                             ".api.cognitive.microsoft.com/speechtotext/"
                             "transcriptions:transcribe?api-version=2025-10-15",
                         config.key, std::move(audio), clean, stop);
}

std::pair<std::string, double> transcribe_at(const std::string& url, const std::string& key,
                                             audio::Receiver audio, bool clean,
                                             std::stop_token stop) {
    struct Close {
        audio::Receiver& audio;
        ~Close() { audio->close(); }
    } close{audio};
    auto value = trim(key);
    if (!valid_header(value)) throw Error("Invalid Azure API key");
    std::vector<std::uint8_t> pcm;
    while (auto packet = audio->recv(stop)) {
        if (packet->error) throw Error(*packet->error);
        pcm.insert(pcm.end(), packet->bytes.begin(), packet->bytes.end());
    }
    if (stop.stop_requested()) throw Error("Cancelled");
    if (pcm.size() < MIN_BYTES) throw audio::NoSpeech();
    json definition = {{"enhancedMode", {{"enabled", true}, {"model", FAST_MODEL}}}};
    if (clean) definition["enhancedMode"]["modelOptions"] = {{"transcribeStyle", "clean"}};
    auto boundary = uuid_simple();
    auto header = wave_header(static_cast<std::uint32_t>(pcm.size()));
    auto body = "--" + boundary +
                "\r\nContent-Disposition: form-data; name=\"definition\"\r\n\r\n" +
                definition.dump() + "\r\n--" + boundary +
                "\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"audio.wav\"\r\n"
                "Content-Type: audio/wav\r\n\r\n";
    body.append(header.begin(), header.end());
    body.append(pcm.begin(), pcm.end());
    body += "\r\n--" + boundary + "--\r\n";
    http::Response response;
    try {
        response =
            http::send({.method = "POST",
                        .url = url,
                        .headers = {"Ocp-Apim-Subscription-Key: " + value,
                                    "Content-Type: multipart/form-data; boundary=" + boundary},
                        .body = std::move(body),
                        .timeout = 5min},
                       MAX_MESSAGE_BYTES, stop);
    } catch (const http::Failure& failure) {
        if (failure.kind == http::Failure::Kind::Cancelled) throw Error("Cancelled");
        if (failure.kind == http::Failure::Kind::Timeout) throw Error("Azure timed out");
        throw Error("Could not connect to Azure");
    }
    auto reply = json::parse(response.body, nullptr, false);
    if (response.status == 401 || response.status == 403) throw Error("Azure API key rejected");
    if (response.status == 429) throw Error("Azure rate limit reached");
    if (response.status < 200 || response.status > 299) {
        auto failed = "Azure transcription failed (" + std::to_string(response.status) + ")";
        // Speech errors carry their message at the top level or under "error".
        auto message = trim(field(reply, "message"));
        if (message.empty() && reply.is_object() && reply.contains("error"))
            message = trim(field(reply["error"], "message"));
        throw Error(message.empty() ? failed : failed + ": " + message);
    }
    if (!reply.is_object()) throw Error("Invalid Azure response");
    std::string text;
    for (const auto& phrase : reply.value("combinedPhrases", json::array())) {
        auto part = trim(field(phrase, "text"));
        if (part.empty()) continue;
        if (!text.empty()) text += ' ';
        text += part;
    }
    if (text.empty()) throw audio::NoSpeech();
    return {text, static_cast<double>(pcm.size()) / (audio::SAMPLE_RATE * 2)};
}

} // namespace transcribe::azure
