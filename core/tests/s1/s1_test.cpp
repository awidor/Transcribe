#include "s1/engine.hpp"
#include "support/server.hpp"
#include "transcribe/s1.hpp"

#include <archive.h>
#include <archive_entry.h>
#include <gtest/gtest.h>

#include <fstream>
#include <memory>
#include <thread>

using namespace transcribe;
using namespace transcribe::s1;

namespace {

std::string read(const std::filesystem::path& path) {
    std::ifstream file(path, std::ios::binary);
    return std::string(std::istreambuf_iterator<char>(file), {});
}

void write(const std::filesystem::path& path, std::string_view bytes) {
    std::ofstream(path, std::ios::binary) << bytes;
}

Asset asset(std::string url, std::string_view content) {
    Sha256 hash;
    hash.update(content.data(), content.size());
    return {std::move(url), hash.hex(), content.size()};
}

// Serves `content` once, honouring a Range request, and records where the range started.
struct FileServer {
    explicit FileServer(std::string content)
        : peer([this, content = std::move(content)] {
              auto connection = server.accept();
              auto range = connection.read_request().header("range");
              if (range.empty()) return connection.respond(200, content);
              start = std::stoull(range.substr(std::string("bytes=").size()));
              connection.respond(206, content.substr(*start));
          }) {}
    test::Server server;
    std::optional<std::uint64_t> start;
    std::jthread peer;
};

// A zip on Windows and a tar.gz elsewhere, like the llama.cpp releases.
std::filesystem::path pack(const std::filesystem::path& source, std::filesystem::path path) {
    std::unique_ptr<::archive, decltype(&archive_write_free)> writer(archive_write_new(),
                                                                     archive_write_free);
#ifdef _WIN32
    path += ".zip";
    archive_write_set_format_zip(writer.get());
    archive_write_open_filename_w(writer.get(), path.c_str());
#else
    path += ".tar.gz";
    archive_write_add_filter_gzip(writer.get());
    archive_write_set_format_pax_restricted(writer.get());
    archive_write_open_filename(writer.get(), path.c_str());
#endif
    for (const auto& file : std::filesystem::recursive_directory_iterator(source)) {
        std::unique_ptr<archive_entry, decltype(&archive_entry_free)> entry(archive_entry_new(),
                                                                            archive_entry_free);
        auto name = file.path().lexically_relative(source).generic_string();
        archive_entry_set_pathname(entry.get(), name.c_str());
        archive_entry_set_perm(entry.get(), 0755);
        auto content = file.is_directory() ? std::string() : read(file.path());
        archive_entry_set_filetype(entry.get(), file.is_directory() ? AE_IFDIR : AE_IFREG);
        archive_entry_set_size(entry.get(), static_cast<la_int64_t>(content.size()));
        archive_write_header(writer.get(), entry.get());
        archive_write_data(writer.get(), content.data(), content.size());
    }
    archive_write_close(writer.get());
    return path;
}

} // namespace

TEST(S1, DownloadResumesAndVerifiesTheChecksum) {
    const std::string content = "0123456789abcdef";
    test::TempDir dir;
    FileServer file(content);
    write(dir.path() / "file.bin.part", content.substr(0, 6));
    std::uint64_t done = 0;
    fetch(asset(file.server.url() + "/file.bin", content), dir.path() / "file.bin", {},
          [&](std::uint64_t bytes) { done += bytes; });
    file.peer.join();
    EXPECT_EQ(file.start, 6u);
    EXPECT_EQ(read(dir.path() / "file.bin"), content);
    EXPECT_EQ(done, 16u);
    EXPECT_FALSE(std::filesystem::exists(dir.path() / "file.bin.part"));
}

TEST(S1, DownloadWithAWrongChecksumIsDiscarded) {
    test::TempDir dir;
    FileServer file("tampered content");
    try {
        fetch(asset(file.server.url() + "/file.bin", "expected content"), dir.path() / "file.bin",
              {}, [](std::uint64_t) {});
        FAIL() << "expected a failed download";
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Download failed");
    }
    file.peer.join();
    EXPECT_EQ(file.start, std::nullopt);
    EXPECT_FALSE(std::filesystem::exists(dir.path() / "file.bin"));
    EXPECT_FALSE(std::filesystem::exists(dir.path() / "file.bin.part"));
}

TEST(S1, NothingDownloadsOrStartsWithoutAnExplicitDownload) {
    test::TempDir dir;
    auto engine = Engine::create(dir.path());
    auto status = engine->status();
    EXPECT_FALSE(status.engine.ready || status.model.ready);
    EXPECT_EQ(status.engine.progress, std::nullopt);
    std::uint64_t size = 0;
    for (const auto& asset : runtime().assets)
        size += asset.size;
    EXPECT_EQ(status.engine.size, size);
    EXPECT_EQ(status.model.size, MODEL.size);
    try {
        engine->clean("um hello", Styling::SemiFormal);
        FAIL() << "expected a missing download";
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "llama.cpp not downloaded");
    }
    EXPECT_TRUE(std::filesystem::is_empty(dir.path()));
}

TEST(S1, RuntimeInstallsFromFlatAndNestedArchives) {
    test::TempDir dir;
    auto source = dir.path() / "source";
    std::filesystem::create_directories(source / "flat");
    std::filesystem::create_directories(source / "nested" / "llama-b1");
    write(source / "nested" / "llama-b1" / SERVER, "server");
    write(source / "flat" / "cudart.dll", "runtime");
    auto target = dir.path() / "llama";
    install_runtime(
        {pack(source / "nested", dir.path() / "a"), pack(source / "flat", dir.path() / "b")},
        dir.path() / "llama.part", target);
    EXPECT_EQ(read(target / SERVER), "server");
    EXPECT_EQ(read(target / "cudart.dll"), "runtime");
    EXPECT_EQ(std::distance(std::filesystem::directory_iterator(target), {}), 2);
    EXPECT_FALSE(std::filesystem::exists(dir.path() / "llama.part"));
#ifndef _WIN32
    auto permissions = std::filesystem::status(target / SERVER).permissions();
    EXPECT_NE(permissions & std::filesystem::perms::owner_exec, std::filesystem::perms::none);
#endif
}
