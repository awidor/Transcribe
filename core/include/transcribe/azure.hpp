#pragma once

#include "transcribe/audio.hpp"

#include <functional>
#include <memory>
#include <stop_token>
#include <string>
#include <utility>

namespace transcribe::azure {

struct Config {
    std::string region;
    std::string key;
};

// Text Azure has confirmed, followed by its current guess at what comes next.
struct Words {
    std::string text;
    std::string interim;
    bool empty() const { return trim(text).empty() && trim(interim).empty(); }
    bool operator==(const Words&) const = default;
};

// The region an Azure key belongs to. A key is accepted only by its own resource's region,
// so no endpoint needs to be entered.
std::string region(const std::string& key);

class Stream {
  public:
    static Stream connect(const Config& config, std::stop_token stop);
    Stream(Stream&&) noexcept;
    Stream& operator=(Stream&&) noexcept;
    ~Stream();

    // Sends audio until the receiver closes, calling `words` whenever they change, then
    // returns the final transcript and the seconds of audio it covers. Throws
    // audio::NoSpeech for silence.
    std::pair<std::string, double> run(audio::Receiver audio,
                                       const std::function<void(const Words&)>& words,
                                       std::stop_token stop);

    struct Socket;

  private:
    explicit Stream(std::unique_ptr<Socket> socket);
    std::unique_ptr<Socket> socket_;
};

} // namespace transcribe::azure
