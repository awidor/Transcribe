#pragma once

#include "transcribe/util.hpp"

#include <chrono>
#include <functional>
#include <optional>
#include <stop_token>
#include <string>
#include <string_view>
#include <vector>

namespace transcribe::http {

struct Request {
    std::string method = "GET";
    std::string url;
    // Each "Name: value".
    std::vector<std::string> headers;
    std::optional<std::string> body;
    std::chrono::milliseconds connect_timeout{std::chrono::seconds(15)};
    // The whole exchange; zero waits indefinitely.
    std::chrono::milliseconds timeout{0};
};

// No HTTP response arrived: the connection failed, timed out or was cancelled.
struct Failure : Error {
    enum class Kind { Connect, Timeout, Cancelled, Other };
    Failure(Kind kind, const std::string& message) : Error(message), kind(kind) {}
    Kind kind;
};

// Sends `request` and streams the response body to `on_body` with the status code. Returning
// false from `on_body` stops reading and returns the status. Throws Failure without a response.
long perform(const Request& request,
             const std::function<bool(long status, std::string_view chunk)>& on_body,
             std::stop_token stop = {});

struct Response {
    long status = 0;
    std::string body;
};

// Sends `request` and reads the whole body. A body over `limit` bytes throws
// Error("Response is too large").
Response send(const Request& request, std::size_t limit, std::stop_token stop = {});

// Percent-encodes a query component.
std::string escape(std::string_view text);

} // namespace transcribe::http
