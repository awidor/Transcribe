#include "support/server.hpp"

#include "transcribe/util.hpp"

#include <stdexcept>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
using socklen_t = int;
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

namespace transcribe::test {

namespace {

#ifdef _WIN32
struct Winsock {
    Winsock() {
        WSADATA data;
        WSAStartup(MAKEWORD(2, 2), &data);
    }
};
void close_socket(std::intptr_t s) { closesocket(static_cast<SOCKET>(s)); }
bool readable(std::intptr_t s, int milliseconds) {
    WSAPOLLFD fd{static_cast<SOCKET>(s), POLLRDNORM, 0};
    return WSAPoll(&fd, 1, milliseconds) > 0;
}
#else
struct Winsock {};
void close_socket(std::intptr_t s) { ::close(static_cast<int>(s)); }
bool readable(std::intptr_t s, int milliseconds) {
    pollfd fd{static_cast<int>(s), POLLIN, 0};
    return ::poll(&fd, 1, milliseconds) > 0;
}
#endif

void init() {
    static Winsock winsock;
    (void)winsock;
}

} // namespace

std::string HttpRequest::header(std::string_view name) const {
    auto wanted = to_lower_ascii(name);
    for (const auto& [key, value] : headers)
        if (to_lower_ascii(key) == wanted) return value;
    return {};
}

Connection::Connection(std::intptr_t socket) : socket_(socket) {}
Connection::Connection(Connection&& other) noexcept
    : socket_(std::exchange(other.socket_, -1)), buffer_(std::move(other.buffer_)) {}
Connection& Connection::operator=(Connection&& other) noexcept {
    if (this != &other) {
        close();
        socket_ = std::exchange(other.socket_, -1);
        buffer_ = std::move(other.buffer_);
    }
    return *this;
}
Connection::~Connection() { close(); }

void Connection::close() {
    if (socket_ != -1) close_socket(socket_);
    socket_ = -1;
}

std::string Connection::read_some() {
    if (!buffer_.empty()) return std::exchange(buffer_, {});
    char data[65536];
    auto n = ::recv(static_cast<decltype(::socket(0, 0, 0))>(socket_), data, sizeof data, 0);
    return n > 0 ? std::string(data, static_cast<std::size_t>(n)) : std::string();
}

std::string Connection::read_exact(std::size_t size) {
    while (buffer_.size() < size) {
        char data[65536];
        auto n = ::recv(static_cast<decltype(::socket(0, 0, 0))>(socket_), data, sizeof data, 0);
        if (n <= 0) break;
        buffer_.append(data, static_cast<std::size_t>(n));
    }
    auto taken = buffer_.substr(0, std::min(size, buffer_.size()));
    buffer_.erase(0, taken.size());
    return taken;
}

HttpRequest Connection::read_request() {
    std::size_t end;
    while ((end = buffer_.find("\r\n\r\n")) == std::string::npos) {
        char data[65536];
        auto n = ::recv(static_cast<decltype(::socket(0, 0, 0))>(socket_), data, sizeof data, 0);
        if (n <= 0) throw std::runtime_error("Connection closed before a request");
        buffer_.append(data, static_cast<std::size_t>(n));
    }
    auto head = buffer_.substr(0, end);
    buffer_.erase(0, end + 4);
    HttpRequest request;
    std::size_t line_end = head.find("\r\n");
    auto first = head.substr(0, line_end);
    auto space = first.find(' ');
    request.method = first.substr(0, space);
    request.target = first.substr(space + 1, first.rfind(' ') - space - 1);
    std::size_t start = line_end == std::string::npos ? head.size() : line_end + 2;
    while (start < head.size()) {
        auto next = head.find("\r\n", start);
        auto line = head.substr(start, next == std::string::npos ? std::string::npos : next - start);
        auto colon = line.find(':');
        if (colon != std::string::npos)
            request.headers.emplace_back(line.substr(0, colon), trim(line.substr(colon + 1)));
        start = next == std::string::npos ? head.size() : next + 2;
    }
    auto length = request.header("content-length");
    if (!length.empty()) request.body = read_exact(std::stoul(length));
    return request;
}

void Connection::write(std::string_view bytes) {
    while (!bytes.empty()) {
        auto n = ::send(static_cast<decltype(::socket(0, 0, 0))>(socket_), bytes.data(),
                        static_cast<int>(bytes.size()), 0);
        if (n <= 0) return;
        bytes.remove_prefix(static_cast<std::size_t>(n));
    }
}

void Connection::respond(int status, std::string_view body,
                         const std::vector<std::string>& headers) {
    std::string response = "HTTP/1.1 " + std::to_string(status) + " Status\r\n";
    for (const auto& header : headers) response += header + "\r\n";
    response += "Content-Length: " + std::to_string(body.size()) + "\r\nConnection: close\r\n\r\n";
    response += body;
    write(response);
}

Server::Server() {
    init();
    auto s = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    socket_ = static_cast<std::intptr_t>(s);
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = 0;
    if (::bind(s, reinterpret_cast<sockaddr*>(&address), sizeof address) != 0 ||
        ::listen(s, 64) != 0)
        throw std::runtime_error("Test server could not listen");
    socklen_t size = sizeof address;
    ::getsockname(s, reinterpret_cast<sockaddr*>(&address), &size);
    port_ = ntohs(address.sin_port);
}

Server::~Server() { close_socket(socket_); }

Connection Server::accept(std::chrono::milliseconds timeout) {
    if (!readable(socket_, static_cast<int>(timeout.count())))
        throw std::runtime_error("No connection arrived");
    auto s = ::accept(static_cast<decltype(::socket(0, 0, 0))>(socket_), nullptr, nullptr);
    return Connection(static_cast<std::intptr_t>(s));
}

TempDir::TempDir()
    : path_(std::filesystem::temp_directory_path() / ("transcribe-test-" + uuid_simple())) {
    std::filesystem::create_directories(path_);
}

TempDir::~TempDir() {
    std::error_code ignored;
    std::filesystem::remove_all(path_, ignored);
}

} // namespace transcribe::test
