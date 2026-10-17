#include "transcribe/audio.hpp"
#include "audio/pcm.hpp"

// Real devices only: without a sound server or card, capture fails instead of recording
// silence. PipeWire serves PulseAudio clients, so ALSA is only the fallback.
#define MA_ENABLE_ONLY_SPECIFIC_BACKENDS
#define MA_ENABLE_PULSEAUDIO
#define MA_ENABLE_ALSA
#define MA_ENABLE_WASAPI
#define MINIAUDIO_IMPLEMENTATION
#include <miniaudio.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <fstream>
#include <future>
#include <span>
#include <string_view>
#include <thread>

namespace transcribe::audio {

namespace {

// Two soft notes: rising when recording starts, falling when it stops.
std::vector<float> chime_samples(float rate, bool rising) {
    auto [first, second] = rising ? std::pair(659.3f, 880.f) : std::pair(880.f, 659.3f);
    auto note = [](float t, float start, float frequency) {
        t -= start;
        if (t < 0) return 0.f;
        return std::min(t * 250, 1.f) * std::exp(-t * 16) * std::sin(TAU * frequency * t);
    };
    std::vector<float> samples(static_cast<std::size_t>(rate * 0.45f));
    for (std::size_t n = 0; n < samples.size(); ++n) {
        float t = static_cast<float>(n) / rate;
        samples[n] = 0.14f * (note(t, 0, first) + note(t, 0.085f, second));
    }
    return samples;
}

// miniaudio objects point into themselves, so they stay on the heap where they were made.
template <auto uninit> struct Uninit {
    template <class T> void operator()(T* value) const {
        uninit(value);
        delete value;
    }
};
using Context = std::unique_ptr<ma_context, Uninit<ma_context_uninit>>;
using Device = std::unique_ptr<ma_device, Uninit<ma_device_uninit>>;
using Decoder = std::unique_ptr<ma_decoder, Uninit<ma_decoder_uninit>>;

Context open_context() {
    auto context = std::make_unique<ma_context>();
    if (ma_context_init(nullptr, 0, nullptr, context.get()) != MA_SUCCESS)
        throw Error("Microphone unavailable");
    return Context(context.release());
}

std::span<ma_device_info> inputs(ma_context* context) {
    ma_device_info* infos = nullptr;
    ma_uint32 count = 0;
    if (ma_context_get_devices(context, nullptr, nullptr, &infos, &count) != MA_SUCCESS)
        throw Error("Microphone unavailable");
    return {infos, count};
}

// Null when the device cannot be opened. A null context picks the default backend.
Device open_device(ma_context* context, const ma_device_config& config) {
    auto device = std::make_unique<ma_device>();
    if (ma_device_init(context, &config, device.get()) != MA_SUCCESS) return nullptr;
    return Device(device.release());
}

std::vector<float> to_mono(const float* interleaved, std::size_t frames, std::size_t channels) {
    std::vector<float> mono(frames);
    for (auto& sample : mono) {
        float sum = 0;
        for (std::size_t c = 0; c < channels; ++c)
            sum += *interleaved++;
        sample = sum / static_cast<float>(channels);
    }
    return mono;
}

// What the device callbacks share with the recording thread.
struct Capture {
    std::stop_token stopped;
    Channel<std::vector<float>> input{32};
    std::atomic<bool> failed = false;
    ma_uint32 channels = 0;
    ma_uint32 rate = 0;
};

void on_input(ma_device* device, void*, const void* data, ma_uint32 frames) {
    auto& capture = *static_cast<Capture*>(device->pUserData);
    if (capture.failed) return;
    // Bound callback allocations even for unusual device buffers.
    if (frames > capture.rate) {
        capture.failed = true;
        return;
    }
    auto mono = to_mono(static_cast<const float*>(data), frames, capture.channels);
    if (!mono.empty() && !capture.input.try_send(std::move(mono))) capture.failed = true;
}

void on_notification(const ma_device_notification* notification) {
    auto& capture = *static_cast<Capture*>(notification->pDevice->pUserData);
    // Stopping on request notifies too; any other stop means the microphone went away.
    if (notification->type == ma_device_notification_type_stopped &&
        !capture.stopped.stop_requested())
        capture.failed = true;
}

void record(std::stop_token stopped, std::optional<std::string> device_name,
            std::function<void(float)> level, Receiver output, std::promise<void> ready) {
    Capture capture{.stopped = stopped};
    Context context;
    Device device;
    std::optional<PcmConverter> converter;
    try {
        context = open_context();
        auto config = ma_device_config_init(ma_device_type_capture);
        ma_device_id id;
        if (device_name) {
            auto devices = inputs(context.get());
            auto found = std::ranges::find(devices, std::string_view(*device_name),
                                           [](auto& info) { return std::string_view(info.name); });
            if (found == devices.end()) throw Error("Microphone unavailable");
            id = found->id;
            config.capture.pDeviceID = &id;
        }
        // miniaudio converts to float; the device keeps its own rate and channel count.
        config.capture.format = ma_format_f32;
        config.dataCallback = on_input;
        config.notificationCallback = on_notification;
        config.pUserData = &capture;
        device = open_device(context.get(), config);
        if (!device) throw Error("Microphone unavailable");
        capture.channels = device->capture.channels;
        capture.rate = device->sampleRate;
        converter.emplace(capture.rate);
        if (ma_device_start(device.get()) != MA_SUCCESS)
            throw Error("Microphone permission required");
        ready.set_value();
    } catch (const std::exception& e) {
        ready.set_exception(std::make_exception_ptr(Error(e.what())));
        return;
    }
    auto publish = [&](std::vector<std::uint8_t> bytes) {
        if (!bytes.empty() && !output->try_send(Packet{std::move(bytes), std::nullopt}))
            throw Error("The connection could not keep up; recording stopped");
    };
    const auto rate = capture.rate;
    // The level covers every frame since the last report, about 30 times a second.
    Biquad rumble(rate, 200, true), hiss(rate, 4000, false);
    std::uint32_t frames = 0;
    float energy = 0;
    std::optional<std::string> error;
    try {
        while (!stopped.stop_requested()) {
            if (capture.failed)
                throw Error("Microphone disconnected or its audio buffer overflowed");
            auto samples = capture.input.recv_for(std::chrono::milliseconds(20));
            if (!samples) continue;
            for (float sample : *samples) {
                float voice = hiss.process(rumble.process(sample));
                energy += voice * voice;
                ++frames;
            }
            if (frames > rate / 30) {
                level(std::sqrt(energy / static_cast<float>(frames)));
                frames = 0;
                energy = 0;
            }
            publish(converter->push(*samples));
        }
    } catch (const std::exception& e) {
        error = e.what();
    }
    device.reset();
    try {
        if (!error) {
            if (capture.failed)
                throw Error("Microphone disconnected or its audio buffer overflowed");
            while (auto samples = capture.input.try_recv())
                publish(converter->push(*samples));
            publish(converter->finish());
        }
    } catch (const std::exception& e) {
        error = e.what();
    }
    if (error) output->send(Packet{{}, std::move(error)});
    output->close();
}

} // namespace

std::vector<std::string> devices() {
    auto context = open_context();
    std::vector<std::string> names;
    for (auto& info : inputs(context.get()))
        names.emplace_back(info.name);
    return names;
}

std::pair<std::unique_ptr<Recorder>, Receiver>
Recorder::start(std::optional<std::string> device_name, std::function<void(float)> level) {
    std::stop_source stop;
    std::promise<void> ready;
    auto started = ready.get_future();
    // About twenty seconds of packets: speech captured while the connection opens waits
    // here. No audio files or unbounded buffers.
    auto output = std::make_shared<Channel<Packet>>(1024);
    std::thread(record, stop.get_token(), std::move(device_name), std::move(level), output,
                std::move(ready))
        .detach();
    started.get();
    return {std::unique_ptr<Recorder>(new Recorder(std::move(stop))), output};
}

Recorder::~Recorder() {
    stop_.request_stop();
}

Receiver decode(const std::filesystem::path& path) {
    if (!std::ifstream(path)) throw Error("Audio could not be read");
    // Float samples at the file's own rate and channel count.
    auto config = ma_decoder_config_init(ma_format_f32, 0, 0);
    auto opened = std::make_unique<ma_decoder>();
#ifdef _WIN32
    auto result = ma_decoder_init_file_w(path.c_str(), &config, opened.get());
#else
    auto result = ma_decoder_init_file(path.c_str(), &config, opened.get());
#endif
    if (result != MA_SUCCESS) throw Error("Audio format unsupported");
    Decoder decoder(opened.release());
    ma_uint32 channels = 0, rate = 0;
    ma_decoder_get_data_format(decoder.get(), nullptr, &channels, &rate, nullptr, 0);
    PcmConverter converter(rate);
    auto output = std::make_shared<Channel<Packet>>(64);
    std::thread([decoder = std::move(decoder), converter = std::move(converter), output,
                 channels = std::max<std::size_t>(channels, 1)]() mutable {
        // A closed receiver means the import was cancelled.
        auto send = [&](std::vector<std::uint8_t> bytes) {
            return bytes.empty() || output->send(Packet{std::move(bytes), std::nullopt});
        };
        try {
            constexpr ma_uint64 BLOCK = 4096;
            std::vector<float> interleaved(BLOCK * channels);
            for (;;) {
                ma_uint64 frames = 0;
                auto result =
                    ma_decoder_read_pcm_frames(decoder.get(), interleaved.data(), BLOCK, &frames);
                if (result != MA_SUCCESS && result != MA_AT_END)
                    throw Error("Audio could not be read");
                if (frames == 0) break;
                if (!send(converter.push(to_mono(interleaved.data(), frames, channels)))) return;
            }
            send(converter.finish());
        } catch (const std::exception& e) {
            output->send(Packet{{}, e.what()});
        }
        output->close();
    }).detach();
    return output;
}

void chime(bool rising) {
    std::thread([rising] {
        struct Playback {
            std::vector<float> samples;
            std::size_t position = 0;
        } playback;
        auto config = ma_device_config_init(ma_device_type_playback);
        config.playback.format = ma_format_f32;
        config.pUserData = &playback;
        config.dataCallback = [](ma_device* device, void* data, const void*, ma_uint32 frames) {
            auto& playback = *static_cast<Playback*>(device->pUserData);
            auto out = static_cast<float*>(data);
            for (ma_uint32 n = 0; n < frames; ++n, ++playback.position) {
                float sample = playback.position < playback.samples.size()
                                   ? playback.samples[playback.position]
                                   : 0;
                out = std::fill_n(out, device->playback.channels, sample);
            }
        };
        // Missing or busy speakers leave it silent rather than affecting the recording.
        auto device = open_device(nullptr, config);
        if (!device) return;
        auto rate = static_cast<float>(device->sampleRate);
        playback.samples = chime_samples(rate, rising);
        if (ma_device_start(device.get()) != MA_SUCCESS) return;
        std::this_thread::sleep_for(
            std::chrono::duration<float>(static_cast<float>(playback.samples.size()) / rate) +
            std::chrono::milliseconds(150));
    }).detach();
}

} // namespace transcribe::audio
