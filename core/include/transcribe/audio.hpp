#pragma once

#include "transcribe/util.hpp"

#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
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

// A WAV header announcing `data` bytes of mono 16-bit PCM at SAMPLE_RATE; a stream announces
// no length.
inline std::vector<std::uint8_t> wave_header(std::uint32_t data = 0) {
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
    number(SAMPLE_RATE, 4);
    number(SAMPLE_RATE * 2, 4);
    number(2, 2);
    number(16, 2);
    text("data");
    number(data, 4);
    return header;
}

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
    static std::pair<std::unique_ptr<Recorder>, Receiver> start(std::optional<std::string> device,
                                                                std::function<void(float)> level);
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
