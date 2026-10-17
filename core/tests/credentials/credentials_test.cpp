#include "support/server.hpp"
#include "transcribe/credentials.hpp"

#include <gtest/gtest.h>

#include <fstream>

#ifndef _WIN32
#include <sys/stat.h>
#endif

using namespace transcribe;

TEST(Credentials, SavesAndReplacesKeyWithoutAKeyring) {
    test::TempDir dir;
    auto path = dir.path() / "api-key";
    EXPECT_THROW(credentials::read(path), std::exception);
    credentials::save(path, "  first-key\n");
    EXPECT_EQ(credentials::read(path), "first-key");
    credentials::save(path, "replacement");
    EXPECT_EQ(credentials::read(path), "replacement");
    EXPECT_EQ(std::distance(std::filesystem::directory_iterator(dir.path()), {}), 1);
}

TEST(Credentials, EmptyInputPreservesSavedKey) {
    test::TempDir dir;
    auto path = dir.path() / "api-key";
    credentials::save(path, "saved-key");
    EXPECT_THROW(credentials::save(path, " \n\t"), std::exception);
    EXPECT_EQ(credentials::read(path), "saved-key");
}

#ifndef _WIN32
TEST(Credentials, KeyFileIsPrivateEvenWhenReplacingAPublicFile) {
    test::TempDir dir;
    auto path = dir.path() / "api-key";
    std::ofstream(path) << "old-key";
    ::chmod(path.c_str(), 0644);
    credentials::save(path, "new-key");
    struct stat info{};
    ::stat(path.c_str(), &info);
    EXPECT_EQ(info.st_mode & 0777, 0600u);
}
#endif
