#include "support/server.hpp"
#include "transcribe/http.hpp"
#include "transcribe/util.hpp"

#include <gtest/gtest.h>

#include <fstream>
#include <thread>

#ifndef _WIN32
#include <sys/stat.h>
#endif

using namespace transcribe;

TEST(Text, TrimsAndSplitsUnicodeWhitespaceLikeRust) {
    EXPECT_EQ(trim(" \t Grüße 你好　\n"), "Grüße 你好");
    EXPECT_EQ(trim("   "), "");
    auto words = split_whitespace(" one two\n\nthree ");
    ASSERT_EQ(words.size(), 3u);
    EXPECT_EQ(words[0], "one");
    EXPECT_EQ(words[1], "two");
    EXPECT_EQ(words[2], "three");
    EXPECT_TRUE(is_control(U'\u0085'));
    EXPECT_FALSE(is_control(U'é'));
    EXPECT_EQ(encode_utf8(decode_utf8("Grüße 👋")), "Grüße 👋");
    EXPECT_EQ(decode_utf8("\xff"), std::u32string(1, U'�'));
}

TEST(Ids, AreRandomVersion4Uuids) {
    auto id = uuid_v4();
    EXPECT_TRUE(is_uuid(id));
    EXPECT_EQ(id[14], '4');
    EXPECT_NE(id, uuid_v4());
    EXPECT_EQ(uuid_simple().size(), 32u);
    EXPECT_FALSE(is_uuid("unrelated"));
}

TEST(Sha256, MatchesKnownDigests) {
    Sha256 empty;
    EXPECT_EQ(empty.hex(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    Sha256 split;
    split.update("ab", 2);
    split.update("c", 1);
    EXPECT_EQ(split.hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}

TEST(Files, AtomicWriteReplacesAndCanBePrivate) {
    test::TempDir dir;
    auto path = dir.path() / "key";
    write_atomic(path, "first", true);
    write_atomic(path, "second", true);
    std::ifstream file(path);
    std::string text((std::istreambuf_iterator<char>(file)), {});
    EXPECT_EQ(text, "second");
    EXPECT_EQ(std::distance(std::filesystem::directory_iterator(dir.path()), {}), 1);
#ifndef _WIN32
    struct stat info{};
    ::stat(path.c_str(), &info);
    EXPECT_EQ(info.st_mode & 0777, 0600u);
#endif
}

TEST(Channel, DeliversInOrderAndDrainsAfterClose) {
    Channel<int> channel(2);
    EXPECT_TRUE(channel.try_send(1));
    EXPECT_TRUE(channel.try_send(2));
    EXPECT_FALSE(channel.try_send(3));
    std::jthread producer([&] { channel.send(3); });
    EXPECT_EQ(channel.recv(), 1);
    producer.join();
    channel.close();
    EXPECT_FALSE(channel.send(4));
    EXPECT_EQ(channel.recv(), 2);
    EXPECT_EQ(channel.recv(), 3);
    EXPECT_EQ(channel.recv(), std::nullopt);
}

TEST(Channel, RecvStopsOnRequestAndTimeout) {
    Channel<int> channel(1);
    EXPECT_EQ(channel.recv_for(std::chrono::milliseconds(10)), std::nullopt);
    std::stop_source stop;
    std::jthread canceller([&] {
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        stop.request_stop();
    });
    EXPECT_EQ(channel.recv(stop.get_token()), std::nullopt);
}

TEST(Sleep, EndsEarlyWhenStopped) {
    std::stop_source stop;
    EXPECT_TRUE(sleep_for(stop.get_token(), std::chrono::milliseconds(5)));
    stop.request_stop();
    EXPECT_FALSE(sleep_for(stop.get_token(), std::chrono::seconds(10)));
}

TEST(Http, SendsRequestsAndReadsResponses) {
    test::Server server;
    std::jthread peer([&] {
        auto connection = server.accept();
        auto request = connection.read_request();
        EXPECT_EQ(request.method, "POST");
        EXPECT_EQ(request.target, "/path?q=a%20b");
        EXPECT_EQ(request.header("Authorization"), "Bearer key");
        EXPECT_EQ(request.body, "{\"a\":1}");
        connection.respond(201, "created");
    });
    http::Request request;
    request.method = "POST";
    request.url = server.url() + "/path?q=" + http::escape("a b");
    request.headers = {"Authorization: Bearer key", "Content-Type: application/json"};
    request.body = "{\"a\":1}";
    auto response = http::send(request, 1024);
    EXPECT_EQ(response.status, 201);
    EXPECT_EQ(response.body, "created");
}

TEST(Http, LimitsBodiesAndReportsMissingServers) {
    test::Server server;
    std::jthread peer([&] {
        auto connection = server.accept();
        connection.read_request();
        connection.respond(200, std::string(100, 'x'));
    });
    http::Request request;
    request.url = server.url();
    EXPECT_THROW(
        {
            try {
                http::send(request, 10);
            } catch (const Error& e) {
                EXPECT_STREQ(e.what(), "Response is too large");
                throw;
            }
        },
        Error);
    peer.join();
    request.url = "http://127.0.0.1:1";
    EXPECT_THROW(http::send(request, 10), http::Failure);
}

TEST(Http, CancelsAStalledExchange) {
    test::Server server;
    std::stop_source stop;
    std::jthread peer([&] {
        auto connection = server.accept();
        connection.read_request();
        std::this_thread::sleep_for(std::chrono::milliseconds(100));
        stop.request_stop();
        std::this_thread::sleep_for(std::chrono::milliseconds(500));
    });
    http::Request request;
    request.url = server.url();
    try {
        http::send(request, 10, stop.get_token());
        FAIL() << "expected cancellation";
    } catch (const http::Failure& failure) {
        EXPECT_EQ(failure.kind, http::Failure::Kind::Cancelled);
    }
}
