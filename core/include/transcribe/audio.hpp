#pragma once

#include "transcribe/util.hpp"

#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace transcribe::audio {

inline constexpr unsigned MAX_SECONDS = 300;
inline constexpr std::size_t SAMPLE_RATE = 24'000;

// Mono 16-bit little-endian PCM at SAMPLE_RATE, or the error that ended the stream.
struct Packet {
    std::vector<std::uint8_t> bytes;
    std::optional<std::string> error;
};
using Receiver = std::shared_ptr<Channel<Packet>>;

// A recording with nothing to transcribe. It is an outcome, not a failure.
struct NoSpeech : Error {
    NoSpeech() : Error("No speech detected") {}
};

std::vector<std::string> devices();

// Streams the microphone as packets until destroyed. Destroying it flushes the remaining
// audio and then closes the receiver.
class Recorder {
  public:
    // `level` gets the voice-band loudness about 30 times a second, on the capture thread.
    static std::pair<std::unique_ptr<Recorder>, Receiver>
    start(std::optional<std::string> device, std::function<void(float)> level);
    ~Recorder();
    Recorder(const Recorder&) = delete;
    Recorder& operator=(const Recorder&) = delete;

  private:
    explicit Recorder(std::stop_source stop) : stop_(std::move(stop)) {}
    std::stop_source stop_;
};

// Opens a WAV, MP3 or FLAC file and decodes it as packets on a background thread.
// Unreadable files fail here, before anything is sent. Closing the receiver cancels.
Receiver decode(const std::filesystem::path& path);

// Plays the recording chime in the background: rising on start, falling on stop.
void chime(bool rising);

} // namespace transcribe::audio
