#pragma once

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace transcribe::test {

struct HttpRequest {
    std::string method;
    std::string target;
    std::vector<std::pair<std::string, std::string>> headers;
    std::string body;
    // The first header with this name, compared case-insensitively; empty when absent.
    std::string header(std::string_view name) const;
};

// One accepted loopback connection. Closes when destroyed.
class Connection {
  public:
    explicit Connection(std::intptr_t socket);
    Connection(Connection&& other) noexcept;
    Connection& operator=(Connection&& other) noexcept;
    ~Connection();

    // Reads one HTTP request: the request line, headers and a Content-Length body.
    HttpRequest read_request();
    // Reads exactly `size` bytes; fewer only when the peer closes.
    std::string read_exact(std::size_t size);
    // Whatever arrives next; empty once the peer closes.
    std::string read_some();
    void write(std::string_view bytes);
    // Sends a complete response with Content-Length and closes nothing.
    void respond(int status, std::string_view body, const std::vector<std::string>& headers = {});
    void close();

  private:
    std::intptr_t socket_;
    std::string buffer_;
};

// A TCP server on 127.0.0.1 with a free port.
class Server {
  public:
    Server();
    ~Server();
    Server(const Server&) = delete;
    Server& operator=(const Server&) = delete;
    std::uint16_t port() const { return port_; }
    std::string url() const { return "http://127.0.0.1:" + std::to_string(port_); }
    // Waits for the next connection; throws on timeout.
    Connection accept(std::chrono::milliseconds timeout = std::chrono::seconds(10));

  private:
    std::intptr_t socket_;
    std::uint16_t port_;
};

// A fresh directory, removed with everything in it when destroyed.
class TempDir {
  public:
    TempDir();
    ~TempDir();
    TempDir(const TempDir&) = delete;
    TempDir& operator=(const TempDir&) = delete;
    const std::filesystem::path& path() const { return path_; }

  private:
    std::filesystem::path path_;
};

} // namespace transcribe::test
