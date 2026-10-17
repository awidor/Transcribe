#pragma once

#include "transcribe/audio.hpp"

#include <soxr.h>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <memory>
#include <numbers>
#include <span>
#include <utility>
#include <vector>

namespace transcribe::audio {

inline constexpr float TAU = 2 * std::numbers::pi_v<float>;

// A second-order filter (RBJ biquad, Q = 1/√2). The level meter listens through a high-pass
// and a low-pass pair, so rumble and hiss outside the voice band do not move the widget's
// bars. Recordings are unfiltered.
struct Biquad {
    std::array<float, 3> b;
    std::array<float, 2> a;
    std::array<float, 2> z{};

    Biquad(unsigned rate, float frequency, bool high_pass) {
        frequency = std::min(frequency, static_cast<float>(rate) * 0.45f);
        float omega = TAU * frequency / static_cast<float>(rate);
        float sin = std::sin(omega), cos = std::cos(omega);
        float alpha = sin / std::numbers::sqrt2_v<float>;
        float a0 = 1 + alpha;
        auto [edge, middle] =
            high_pass ? std::pair((1 + cos) / 2, -(1 + cos)) : std::pair((1 - cos) / 2, 1 - cos);
        b = {edge / a0, middle / a0, edge / a0};
        a = {-2 * cos / a0, (1 - alpha) / a0};
    }
    float process(float x) {
        float y = b[0] * x + z[0];
        z[0] = b[1] * x - a[0] * y + z[1];
        z[1] = b[2] * x - a[1] * y;
        return y;
    }
};

// Mono float samples to 16-bit PCM at SAMPLE_RATE. soxr skips its own filter delay and gives
// the same output however the input is split.
class PcmConverter {
  public:
    explicit PcmConverter(std::size_t rate) : input_rate_(rate) {
        soxr_error_t error = nullptr;
        auto quality = soxr_quality_spec(SOXR_HQ, 0);
        resampler_.reset(soxr_create(static_cast<double>(rate), SAMPLE_RATE, 1, &error, nullptr,
                                     &quality, nullptr));
        if (error) throw Error(error);
    }
    std::vector<std::uint8_t> push(std::span<const float> samples) {
        input_frames_ += samples.size();
        std::vector<std::uint8_t> bytes;
        // A null input would flush soxr.
        if (!samples.empty()) process(samples.data(), samples.size(), bytes);
        return bytes;
    }
    std::vector<std::uint8_t> finish() {
        std::vector<std::uint8_t> bytes;
        // soxr rounds the flushed length to the nearest frame, never below the target.
        process(nullptr, 0, bytes);
        return bytes;
    }

  private:
    // soxr returns what it has up to the buffer's size, so a full buffer may mean more waits.
    void process(const float* input, std::size_t frames, std::vector<std::uint8_t>& bytes) {
        std::array<float, 4096> out;
        std::size_t done = 0;
        do {
            if (auto error = soxr_process(resampler_.get(), input, frames, nullptr, out.data(),
                                          out.size(), &done))
                throw Error(error);
            encode(std::span(out).first(done), bytes);
            frames = 0;
        } while (done == out.size());
    }
    void encode(std::span<const float> samples, std::vector<std::uint8_t>& bytes) {
        auto remaining = input_frames_ * SAMPLE_RATE / input_rate_ - output_frames_;
        for (float s : samples.first(std::min(remaining, samples.size()))) {
            // Like Rust's `as`, NaN becomes silence instead of undefined behaviour.
            auto value = static_cast<std::uint16_t>(
                std::isnan(s) ? 0 : static_cast<std::int16_t>(std::clamp(s, -1.f, 1.f) * 32767));
            bytes.push_back(static_cast<std::uint8_t>(value));
            bytes.push_back(static_cast<std::uint8_t>(value >> 8));
            ++output_frames_;
        }
    }

    std::unique_ptr<soxr, decltype(&soxr_delete)> resampler_{nullptr, soxr_delete};
    std::size_t input_rate_;
    std::size_t input_frames_ = 0;
    std::size_t output_frames_ = 0;
};

} // namespace transcribe::audio
