#include "audio/pcm.hpp"
#include "support/server.hpp"
#include "transcribe/audio.hpp"

#include <gtest/gtest.h>

using namespace transcribe;
using namespace transcribe::audio;

namespace {

float level(float frequency) {
    const unsigned rate = 48'000;
    Biquad rumble(rate, 200, true), hiss(rate, 4000, false);
    float energy = 0;
    for (unsigned n = 0; n < rate; ++n) {
        float x = std::sin(TAU * frequency * static_cast<float>(n) / static_cast<float>(rate));
        float y = hiss.process(rumble.process(x));
        if (n >= rate / 10) energy += y * y;
    }
    return std::sqrt(energy / (static_cast<float>(rate) * 0.9f));
}

} // namespace

TEST(Audio, LevelMeterHearsTheVoiceBandButNotRumbleOrHiss) {
    float voice = level(1000);
    EXPECT_LT(std::abs(voice - 1 / std::numbers::sqrt2_v<float>), 0.05f);
    // Mains hum and high hiss are at least 18 dB down; low rumble, which overlaps deep voices,
    // about 12 dB.
    for (float frequency : {50.f, 60.f, 12'000.f})
        EXPECT_LT(level(frequency), voice / 8) << frequency << " Hz";
    EXPECT_LT(level(100), voice / 3.5f);
}

TEST(Audio, ResamplingKeepsDurationAndTailAcrossCallbackBoundaries) {
    for (std::size_t rate : {16'000, 24'000, 44'100, 48'000, 96'000}) {
        std::vector<float> samples(rate + 317);
        for (std::size_t n = 0; n < samples.size(); ++n)
            samples[n] =
                std::sin(static_cast<float>(n) * 1000.f * TAU / static_cast<float>(rate)) * 0.5f;
        auto convert = [&](std::size_t chunk) {
            PcmConverter c(rate);
            std::vector<std::uint8_t> bytes;
            for (std::size_t start = 0; start < samples.size(); start += chunk) {
                auto s = std::span(samples).subspan(start, std::min(chunk, samples.size() - start));
                auto out = c.push(s);
                bytes.insert(bytes.end(), out.begin(), out.end());
            }
            auto out = c.finish();
            bytes.insert(bytes.end(), out.begin(), out.end());
            return bytes;
        };
        auto a = convert(137);
        EXPECT_EQ(a, convert(2048)) << rate;
        ASSERT_EQ(a.size(), samples.size() * SAMPLE_RATE / rate * 2) << rate;
        float energy = 0;
        for (std::size_t i = a.size() - 480; i < a.size(); i += 2) {
            float s = static_cast<float>(static_cast<std::int16_t>(a[i] | a[i + 1] << 8)) / 32768;
            energy += s * s;
        }
        EXPECT_GT(energy / 240, 0.05f) << rate;
    }
}

TEST(Audio, ImportsDecodeToMonoPcmAtTheStreamRate) {
    test::TempDir dir;
    auto path = dir.path() / "tone.wav";
    // One second of 44.1 kHz stereo PCM16.
    const std::uint32_t frames = 44'100;
    std::string wav;
    auto put = [&](auto value) {
        for (std::size_t i = 0; i < sizeof value; ++i)
            wav.push_back(static_cast<char>(value >> (8 * i)));
    };
    wav += "RIFF";
    put(36 + frames * 4);
    wav += "WAVEfmt ";
    put(std::uint32_t{16});
    put(std::uint16_t{1}); // PCM
    put(std::uint16_t{2}); // channels
    put(std::uint32_t{44'100});
    put(std::uint32_t{44'100 * 4});
    put(std::uint16_t{4});
    put(std::uint16_t{16});
    wav += "data";
    put(frames * 4);
    for (std::uint32_t n = 0; n < frames; ++n) {
        auto s = static_cast<std::int16_t>(
            std::sin(static_cast<float>(n) * 440.f * TAU / 44'100.f) * 8000.f);
        put(s);
        put(s);
    }
    write_atomic(path, wav, false);
    auto audio = decode(path);
    std::size_t bytes = 0;
    while (auto packet = audio->recv()) {
        ASSERT_FALSE(packet->error) << *packet->error;
        bytes += packet->bytes.size();
    }
    EXPECT_EQ(bytes, SAMPLE_RATE * 2);

    auto message = [](const std::filesystem::path& path) -> std::string {
        try {
            decode(path);
        } catch (const Error& e) {
            return e.what();
        }
        return "decoded";
    };
    write_atomic(path, "not audio", false);
    EXPECT_EQ(message(path), "Audio format unsupported");
    EXPECT_EQ(message(dir.path() / "missing.wav"), "Audio could not be read");
}
