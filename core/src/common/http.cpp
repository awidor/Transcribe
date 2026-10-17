#include "transcribe/http.hpp"

#include <curl/curl.h>

#include <memory>

namespace transcribe::http {

namespace {

struct Global {
    Global() { curl_global_init(CURL_GLOBAL_DEFAULT); }
};

struct Exchange {
    const std::function<bool(long, std::string_view)>* on_body;
    CURL* curl;
    std::stop_token stop;
    bool stopped = false;
};

size_t write(char* data, size_t size, size_t count, void* user) {
    auto* exchange = static_cast<Exchange*>(user);
    long status = 0;
    curl_easy_getinfo(exchange->curl, CURLINFO_RESPONSE_CODE, &status);
    if (!(*exchange->on_body)(status, std::string_view(data, size * count))) {
        exchange->stopped = true;
        return CURL_WRITEFUNC_ERROR;
    }
    return size * count;
}

int progress(void* user, curl_off_t, curl_off_t, curl_off_t, curl_off_t) {
    return static_cast<Exchange*>(user)->stop.stop_requested() ? 1 : 0;
}

} // namespace

long perform(const Request& request,
             const std::function<bool(long status, std::string_view chunk)>& on_body,
             std::stop_token stop) {
    static Global global;
    std::unique_ptr<CURL, decltype(&curl_easy_cleanup)> curl(curl_easy_init(), curl_easy_cleanup);
    if (!curl) throw Failure(Failure::Kind::Other, "HTTP unavailable");
    std::unique_ptr<curl_slist, decltype(&curl_slist_free_all)> headers(nullptr,
                                                                        curl_slist_free_all);
    for (const auto& header : request.headers) {
        auto* next = curl_slist_append(headers.get(), header.c_str());
        if (!next) throw Failure(Failure::Kind::Other, "HTTP unavailable");
        // Appending returns the head of the list, new only for the first header.
        if (!headers) headers.reset(next);
    }
    Exchange exchange{&on_body, curl.get(), stop};
    auto* c = curl.get();
    curl_easy_setopt(c, CURLOPT_URL, request.url.c_str());
    curl_easy_setopt(c, CURLOPT_CUSTOMREQUEST, request.method.c_str());
    if (request.method == "HEAD") curl_easy_setopt(c, CURLOPT_NOBODY, 1L);
    if (request.body) {
        curl_easy_setopt(c, CURLOPT_POSTFIELDS, request.body->data());
        curl_easy_setopt(c, CURLOPT_POSTFIELDSIZE_LARGE,
                         static_cast<curl_off_t>(request.body->size()));
    }
    curl_easy_setopt(c, CURLOPT_HTTPHEADER, headers.get());
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 10L);
    curl_easy_setopt(c, CURLOPT_ACCEPT_ENCODING, "");
    curl_easy_setopt(c, CURLOPT_NOSIGNAL, 1L);
    if (!request.proxy) curl_easy_setopt(c, CURLOPT_NOPROXY, "*");
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT_MS,
                     static_cast<long>(request.connect_timeout.count()));
    curl_easy_setopt(c, CURLOPT_TIMEOUT_MS, static_cast<long>(request.timeout.count()));
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, write);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, &exchange);
    curl_easy_setopt(c, CURLOPT_XFERINFOFUNCTION, progress);
    curl_easy_setopt(c, CURLOPT_XFERINFODATA, &exchange);
    curl_easy_setopt(c, CURLOPT_NOPROGRESS, 0L);
#ifdef _WIN32
    // Schannel fails when a revocation server is unreachable, as behind many proxies.
    curl_easy_setopt(c, CURLOPT_SSL_OPTIONS, static_cast<long>(CURLSSLOPT_REVOKE_BEST_EFFORT));
#endif
    auto result = curl_easy_perform(c);
    long status = 0;
    curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &status);
    if (result == CURLE_OK || (exchange.stopped && result == CURLE_WRITE_ERROR)) return status;
    switch (result) {
    case CURLE_ABORTED_BY_CALLBACK:
        throw Failure(Failure::Kind::Cancelled, "Cancelled");
    case CURLE_OPERATION_TIMEDOUT:
        throw Failure(Failure::Kind::Timeout, curl_easy_strerror(result));
    case CURLE_COULDNT_RESOLVE_HOST:
    case CURLE_COULDNT_RESOLVE_PROXY:
    case CURLE_COULDNT_CONNECT:
    case CURLE_SSL_CONNECT_ERROR:
    case CURLE_PEER_FAILED_VERIFICATION:
        throw Failure(Failure::Kind::Connect, curl_easy_strerror(result));
    default:
        throw Failure(Failure::Kind::Other, curl_easy_strerror(result));
    }
}

Response send(const Request& request, std::size_t limit, std::stop_token stop) {
    Response response;
    bool too_large = false;
    response.status = perform(
        request,
        [&](long, std::string_view chunk) {
            if (response.body.size() + chunk.size() > limit) {
                too_large = true;
                return false;
            }
            response.body.append(chunk);
            return true;
        },
        stop);
    if (too_large) throw Error("Response is too large");
    return response;
}

std::string escape(std::string_view text) {
    static constexpr char digits[] = "0123456789ABCDEF";
    std::string out;
    for (unsigned char c : text) {
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
            c == '-' || c == '_' || c == '.' || c == '~') {
            out += static_cast<char>(c);
        } else {
            out += '%';
            out += digits[c >> 4];
            out += digits[c & 0xf];
        }
    }
    return out;
}

} // namespace transcribe::http
