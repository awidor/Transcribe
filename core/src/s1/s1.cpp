#include "transcribe/s1.hpp"

#include "s1/engine.hpp"
#include "transcribe/http.hpp"

#include <archive.h>
#include <archive_entry.h>
#include <nlohmann/json.hpp>

#include <algorithm>
#include <atomic>
#include <cmath>
#include <condition_variable>
#include <fstream>
#include <limits>
#include <mutex>
#include <thread>

namespace transcribe::s1 {

const Asset MODEL{"https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/"
                  "34add00a48a2e5d24e5a4ee5405a99620a3a240c/s1-mini-q4_k_m.gguf",
                  "3b41ebe2502cbd03e811d5d16b022f5ab551eda58d62597d152f89535003c634", 484'219'808};

namespace {

// The model was trained on exactly this system prompt and control line; the empty think block
// keeps Qwen3's thinking mode off.
constexpr std::string_view SYSTEM =
    "You are a text normalizer for speech-to-text transcripts. The input begins with a control "
    "line specifying the styling, structure, and context settings; clean the transcript to match "
    "those settings and output only the cleaned text.";

std::string prompt(Styling styling, std::string_view transcript) {
    // Transcripts are data: they must never open or close a chat turn.
    std::string text;
    for (std::size_t i = 0; i < transcript.size(); ++i) {
        text += transcript[i];
        if (transcript[i] == '<' && i + 1 < transcript.size() && transcript[i + 1] == '|') ++i;
    }
    return "<|im_start|>system\n" + std::string(SYSTEM) +
           "<|im_end|>\n<|im_start|>user\n[Styling: " + nlohmann::json(styling).get<std::string>() +
           "] [Structure: prose] [Context: general]\n" + text +
           "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n";
}

// Roughly 700 tokens of English; the model is built for passes under 1,000.
constexpr std::size_t CHUNK = 3000;

// The code point starting at byte `i` and its length in bytes.
std::pair<char32_t, std::size_t> code_point(std::string_view text, std::size_t i) {
    auto byte = static_cast<unsigned char>(text[i]);
    std::size_t length = byte < 0xc0 ? 1 : byte < 0xe0 ? 2 : byte < 0xf0 ? 3 : 4;
    auto decoded = decode_utf8(text.substr(i, length));
    if (decoded.size() != 1) return {U'�', 1};
    return {decoded[0], length};
}

std::string_view trim_start(std::string_view text) {
    std::size_t i = 0;
    while (i < text.size()) {
        auto [c, length] = code_point(text, i);
        if (!is_whitespace(c)) break;
        i += length;
    }
    return text.substr(i);
}

std::vector<std::string> chunks(std::string_view text, std::size_t limit) {
    std::vector<std::string> chunks;
    auto rest = trim_start(text);
    rest = rest.substr(0, trim(rest).size());
    while (rest.size() > limit) {
        auto end = limit;
        while (end > 0 && (static_cast<unsigned char>(rest[end]) & 0xc0) == 0x80)
            --end;
        auto window = rest.substr(0, end);
        // The last sentence end followed by whitespace, else the last whitespace.
        std::size_t sentence = 0, space = 0;
        for (std::size_t i = 0; i < window.size();) {
            auto [c, length] = code_point(window, i);
            if (is_whitespace(c)) {
                space = i;
                if (i > 0 && (window[i - 1] == '.' || window[i - 1] == '!' || window[i - 1] == '?'))
                    sentence = i;
            }
            i += length;
        }
        auto cut = sentence > 0 ? sentence : space > 0 ? space : end;
        chunks.push_back(trim(rest.substr(0, cut)));
        rest = trim_start(rest.substr(cut));
    }
    if (!rest.empty()) chunks.emplace_back(rest);
    return chunks;
}

constexpr const char* DOWNLOAD_FAILED = "Download failed";
// Longer than a full-length recording and its transcription.
constexpr std::chrono::minutes WARM{10};
// The Rust downloads client had no overall limit, but its reads timed out.
constexpr std::chrono::seconds READ_TIMEOUT{60};

Download& slot(Status& status, Part part) {
    return part == Part::Engine ? status.engine : status.model;
}

struct Plan {
    const Runtime* runtime;
    std::filesystem::path directory;
    std::filesystem::path model;

    std::vector<const Asset*> missing(Part part) const {
        std::error_code ignored;
        std::vector<const Asset*> missing;
        if (part == Part::Engine && !std::filesystem::exists(directory / SERVER, ignored))
            for (const auto& asset : runtime->assets)
                missing.push_back(&asset);
        if (part == Part::Model && !std::filesystem::exists(model, ignored))
            missing.push_back(&MODEL);
        return missing;
    }
};

struct Server {
    std::string url;
    std::string key;
    std::unique_ptr<Process> child;
};

// llama-server on loopback.
http::Request local(std::string url) {
    http::Request request;
    request.url = std::move(url);
    request.connect_timeout = std::chrono::seconds(5);
    request.timeout = std::chrono::seconds(120);
    request.proxy = false;
    return request;
}

std::string utf8(const std::filesystem::path& path) {
    auto text = path.u8string();
    return std::string(text.begin(), text.end());
}

#ifdef _WIN32
using Native = const wchar_t*;
Native pathname(archive_entry* entry) {
    return archive_entry_pathname_w(entry);
}
Native hardlink(archive_entry* entry) {
    return archive_entry_hardlink_w(entry);
}
void set_pathname(archive_entry* entry, Native path) {
    archive_entry_copy_pathname_w(entry, path);
}
void set_hardlink(archive_entry* entry, Native path) {
    archive_entry_copy_hardlink_w(entry, path);
}
int open_archive(archive* reader, const std::filesystem::path& path) {
    return archive_read_open_filename_w(reader, path.c_str(), 1 << 16);
}
#else
using Native = const char*;
Native pathname(archive_entry* entry) {
    return archive_entry_pathname(entry);
}
Native hardlink(archive_entry* entry) {
    return archive_entry_hardlink(entry);
}
void set_pathname(archive_entry* entry, Native path) {
    archive_entry_copy_pathname(entry, path);
}
void set_hardlink(archive_entry* entry, Native path) {
    archive_entry_copy_hardlink(entry, path);
}
int open_archive(archive* reader, const std::filesystem::path& path) {
    return archive_read_open_filename(reader, path.c_str(), 1 << 16);
}
#endif

// Where an entry lands under `into`; absolute paths are refused.
std::filesystem::path under(const std::filesystem::path& into, Native name) {
    if (!name) throw Error(DOWNLOAD_FAILED);
    std::filesystem::path relative(name);
    if (relative.has_root_path()) throw Error(DOWNLOAD_FAILED);
    return into / relative;
}

// Unpacks a zip (Windows) or tar.gz (Linux) llama.cpp release. Archives match their pinned
// checksums before this runs, so refusing `..` and absolute paths is enough.
void unpack(const std::filesystem::path& file, const std::filesystem::path& into) {
    std::unique_ptr<archive, decltype(&archive_read_free)> reader(archive_read_new(),
                                                                  archive_read_free);
    std::unique_ptr<archive, decltype(&archive_write_free)> writer(archive_write_disk_new(),
                                                                   archive_write_free);
    if (!reader || !writer) throw Error(DOWNLOAD_FAILED);
    archive_read_support_filter_gzip(reader.get());
    archive_read_support_format_tar(reader.get());
    archive_read_support_format_zip(reader.get());
    archive_write_disk_set_options(writer.get(),
                                   ARCHIVE_EXTRACT_TIME | ARCHIVE_EXTRACT_SECURE_NODOTDOT);
    if (open_archive(reader.get(), file) != ARCHIVE_OK) throw Error(DOWNLOAD_FAILED);
    std::filesystem::create_directories(into);
    for (;;) {
        archive_entry* entry;
        auto result = archive_read_next_header(reader.get(), &entry);
        if (result == ARCHIVE_EOF) break;
        if (result < ARCHIVE_WARN) throw Error(DOWNLOAD_FAILED);
        set_pathname(entry, under(into, pathname(entry)).c_str());
        if (auto link = hardlink(entry)) set_hardlink(entry, under(into, link).c_str());
        if (archive_write_header(writer.get(), entry) < ARCHIVE_WARN) throw Error(DOWNLOAD_FAILED);
        const void* block;
        std::size_t size;
        la_int64_t offset;
        while ((result = archive_read_data_block(reader.get(), &block, &size, &offset)) ==
                   ARCHIVE_OK ||
               result == ARCHIVE_WARN)
            if (archive_write_data_block(writer.get(), block, size, offset) < ARCHIVE_WARN)
                throw Error(DOWNLOAD_FAILED);
        if (result != ARCHIVE_EOF || archive_write_finish_entry(writer.get()) < ARCHIVE_WARN)
            throw Error(DOWNLOAD_FAILED);
    }
    if (archive_write_close(writer.get()) != ARCHIVE_OK) throw Error(DOWNLOAD_FAILED);
}

std::string complete(const std::string& url, const std::string& key, const std::string& prompt,
                     std::size_t budget, std::stop_token stop) {
    auto request = local(url + "/completion");
    request.method = "POST";
    request.headers = {"Authorization: Bearer " + key, "Content-Type: application/json"};
    nlohmann::json body = {
        {"prompt", prompt}, {"n_predict", budget}, {"temperature", 0}, {"cache_prompt", true}};
    request.body = body.dump(-1, ' ', false, nlohmann::json::error_handler_t::replace);
    http::Response response;
    try {
        response = http::send(request, 16 * 1024 * 1024, stop);
    } catch (const http::Failure&) {
        throw Error("Cleanup connection failed");
    }
    if (response.status < 200 || response.status > 299)
        throw Error("Cleanup failed (" + std::to_string(response.status) + ")");
    auto value = nlohmann::json::parse(response.body, nullptr, false);
    if (value.is_discarded()) throw Error("Invalid cleanup response");
    auto stop_type =
        value.is_object() ? value.value("stop_type", nlohmann::json()) : nlohmann::json();
    if (stop_type != "eos" && stop_type != "word") throw Error("Cleanup did not finish");
    if (!value.contains("content") || !value["content"].is_string())
        throw Error("Cleanup response contained no text");
    return trim(value["content"].get<std::string>());
}

} // namespace

void install_runtime(const std::vector<std::filesystem::path>& archives,
                     const std::filesystem::path& staging, const std::filesystem::path& target) {
    std::error_code ignored;
    std::filesystem::remove_all(staging, ignored);
    for (std::size_t index = 0; index < archives.size(); ++index) {
        auto unpacked = staging / ("." + std::to_string(index));
        try {
            unpack(archives[index], unpacked);
        } catch (const std::exception&) {
            throw Error(DOWNLOAD_FAILED);
        }
        // Archives hold their files either directly or inside one folder.
        std::vector<std::filesystem::directory_entry> entries(
            std::filesystem::directory_iterator(unpacked), {});
        auto root =
            entries.size() == 1 && std::filesystem::is_directory(entries[0].symlink_status())
                ? entries[0].path()
                : unpacked;
        for (const auto& entry : std::filesystem::directory_iterator(root))
            std::filesystem::rename(entry.path(), staging / entry.path().filename());
        std::filesystem::remove_all(unpacked);
    }
    if (!std::filesystem::exists(staging / SERVER)) throw Error(DOWNLOAD_FAILED);
    std::filesystem::remove_all(target, ignored);
    std::filesystem::rename(staging, target);
}

void fetch(const Asset& asset, const std::filesystem::path& destination, std::stop_token cancel,
           const std::function<void(std::uint64_t)>& progress) {
    auto part = destination.parent_path() / (asset.file() + ".part");
    // An interrupted download resumes after re-hashing what it already has.
    std::optional<Sha256> hasher;
    hasher.emplace();
    std::uint64_t offset = 0;
    if (std::ifstream existing(part, std::ios::binary); existing) {
        std::vector<char> buffer(1 << 16);
        while (existing.read(buffer.data(), static_cast<std::streamsize>(buffer.size())) ||
               existing.gcount() > 0) {
            hasher->update(buffer.data(), static_cast<std::size_t>(existing.gcount()));
            offset += static_cast<std::uint64_t>(existing.gcount());
        }
        if (existing.bad()) throw Error(DOWNLOAD_FAILED);
    }
    if (offset > asset.size) {
        hasher.emplace();
        offset = 0;
    }
    if (offset < asset.size) {
        http::Request request;
        request.url = asset.url;
        if (offset > 0) request.headers.push_back("Range: bytes=" + std::to_string(offset) + "-");
        std::ofstream file;
        // The response's status decides whether the download resumes or starts over.
        auto begin = [&](long status) {
            if (status == 206 && offset > 0) {
                file.open(part, std::ios::binary | std::ios::app);
            } else if (status == 200) {
                hasher.emplace();
                offset = 0;
                file.open(part, std::ios::binary | std::ios::trunc);
            }
            if (!file.is_open()) return false;
            progress(offset);
            return true;
        };
        // A read that stalls this long fails the download instead of hanging it.
        std::stop_source abort;
        std::stop_callback forward(cancel, [&] { abort.request_stop(); });
        std::atomic<std::chrono::steady_clock::rep> last =
            std::chrono::steady_clock::now().time_since_epoch().count();
        std::atomic<bool> stalled = false;
        std::jthread watchdog([&](std::stop_token done) {
            while (sleep_for(done, std::chrono::seconds(1))) {
                auto idle = std::chrono::steady_clock::now().time_since_epoch() -
                            std::chrono::steady_clock::duration(last.load());
                if (idle > READ_TIMEOUT) {
                    stalled = true;
                    abort.request_stop();
                }
            }
        });
        std::optional<std::string> failure;
        long status;
        try {
            status = http::perform(
                request,
                [&](long status, std::string_view chunk) {
                    last = std::chrono::steady_clock::now().time_since_epoch().count();
                    if (cancel.stop_requested())
                        failure = "Cancelled";
                    else if (!file.is_open() && !begin(status))
                        failure = DOWNLOAD_FAILED;
                    else if ((offset += chunk.size()) > asset.size)
                        failure = DOWNLOAD_FAILED;
                    else if (!file.write(chunk.data(), static_cast<std::streamsize>(chunk.size())))
                        failure = DOWNLOAD_FAILED;
                    if (failure) return false;
                    hasher->update(chunk.data(), chunk.size());
                    progress(chunk.size());
                    return true;
                },
                abort.get_token());
        } catch (const http::Failure& error) {
            if (error.kind == http::Failure::Kind::Cancelled && !stalled) throw Error("Cancelled");
            throw Error(DOWNLOAD_FAILED);
        }
        if (failure) throw Error(*failure);
        if (!file.is_open() && !begin(status)) throw Error(DOWNLOAD_FAILED);
        file.close();
        if (!file) throw Error(DOWNLOAD_FAILED);
    } else {
        progress(offset);
    }
    if (offset != asset.size || hasher->hex() != asset.sha256) {
        std::error_code ignored;
        std::filesystem::remove(part, ignored);
        throw Error(DOWNLOAD_FAILED);
    }
    std::filesystem::rename(part, destination);
}

struct Engine::State {
    std::filesystem::path dir_;
    std::mutex status_mutex_;
    Status status_;
    Channel<Status> changes_{std::numeric_limits<std::size_t>::max()};
    std::mutex listeners_mutex_;
    std::vector<std::function<void(const Status&)>> listeners_;
    std::mutex downloading_[2];
    std::mutex cancel_mutex_;
    std::stop_source cancel_[2];
    std::timed_mutex starting_;
    std::mutex server_mutex_;
    std::shared_ptr<Server> server_;
    std::atomic<bool> cpu_only_ = false;
    std::mutex unload_mutex_;
    std::condition_variable_any touched_;
    std::uint64_t uses_ = 0;
    std::optional<std::chrono::steady_clock::duration> unload_ = std::chrono::minutes(5);
    std::optional<std::chrono::steady_clock::time_point> deadline_;
    // Last, so they stop before anything they use is destroyed.
    std::jthread notifier_;
    std::jthread unloader_;

    explicit State(std::filesystem::path dir) : dir_(std::move(dir)) {
        notifier_ = std::jthread([this](std::stop_token stop) {
            while (auto status = changes_.recv(stop)) {
                std::vector<std::function<void(const Status&)>> listeners;
                {
                    std::lock_guard lock(listeners_mutex_);
                    listeners = listeners_;
                }
                for (const auto& listener : listeners)
                    listener(*status);
            }
        });
        unloader_ = std::jthread([this](std::stop_token stop) { unload_when_idle(stop); });
    }

    // Applies `change` and tells the listeners when it changed anything, or always.
    Status modify(const std::function<void(Status&)>& change, bool always) {
        std::lock_guard lock(status_mutex_);
        auto before = status_;
        change(status_);
        if (always || status_ != before) changes_.try_send(status_);
        return status_;
    }

    // What is downloaded, read from disk except while a part downloads.
    Status status() {
        std::optional<Plan> plan;
        std::string error;
        try {
            plan = this->plan();
        } catch (const std::exception& e) {
            error = e.what();
        }
        return modify(
            [&](Status& status) {
                for (auto part : {Part::Engine, Part::Model}) {
                    auto& download = slot(status, part);
                    if (download.progress) continue;
                    if (plan) {
                        auto missing = plan->missing(part);
                        download.ready = missing.empty();
                        download.size = 0;
                        for (const auto* asset : missing)
                            download.size += asset->size;
                    } else {
                        download.ready = false;
                        download.error = error;
                    }
                }
            },
            false);
    }

    void check() {
        auto plan = this->plan();
        if (!plan.missing(Part::Engine).empty()) throw Error("llama.cpp not downloaded");
        if (!plan.missing(Part::Model).empty()) throw Error("S1-mini not downloaded");
    }

    // A second call while the part downloads returns at once.
    void download(Part part) {
        auto index = static_cast<int>(part);
        std::stop_token cancel;
        {
            std::lock_guard lock(cancel_mutex_);
            cancel = cancel_[index].get_token();
        }
        std::unique_lock downloading(downloading_[index], std::try_to_lock);
        if (!downloading) return;
        std::exception_ptr failure;
        std::string message;
        try {
            download_part(part, cancel);
        } catch (const std::exception& error) {
            failure = std::current_exception();
            message = error.what();
        }
        modify(
            [&](Status& status) {
                auto& download = slot(status, part);
                download.progress.reset();
                download.error.reset();
                if (failure && !cancel.stop_requested()) download.error = message;
            },
            true);
        status();
        if (failure) std::rethrow_exception(failure);
    }

    // A cancelled download resumes where it stopped.
    void cancel_download(Part part) {
        std::stop_source cancelled;
        {
            std::lock_guard lock(cancel_mutex_);
            cancelled = std::exchange(cancel_[static_cast<int>(part)], std::stop_source());
        }
        cancelled.request_stop();
    }

    void stop() {
        std::shared_ptr<Server> stopped;
        std::lock_guard lock(server_mutex_);
        stopped = std::move(server_);
    }

    void set_unload(std::optional<std::chrono::steady_clock::duration> unload) {
        {
            std::lock_guard lock(unload_mutex_);
            unload_ = unload;
        }
        if (running()) touch(false);
    }

    std::string clean(const std::string& transcript, Styling styling, std::stop_token stop) {
        auto server = this->server(stop);
        std::vector<std::string> cleaned;
        std::exception_ptr failure;
        for (const auto& chunk : chunks(transcript, CHUNK)) {
            try {
                auto text = complete(server->url, server->key, prompt(styling, chunk),
                                     chunk.size() / 2 + 32, stop);
                if (!text.empty()) cleaned.push_back(std::move(text));
            } catch (...) {
                failure = std::current_exception();
                break;
            }
        }
        touch(false);
        if (failure) std::rethrow_exception(failure);
        std::string joined;
        for (const auto& text : cleaned)
            joined += (joined.empty() ? "" : " ") + text;
        return joined;
    }

    // Unloads the model once it has gone unused for the configured time. A model loaded for a
    // recording outlasts that recording and its transcription, so it is never unloaded before
    // its cleanup.
    void touch(bool warming) {
        std::lock_guard lock(unload_mutex_);
        ++uses_;
        deadline_.reset();
        if (unload_)
            deadline_ = std::chrono::steady_clock::now() +
                        (warming ? std::max<std::chrono::steady_clock::duration>(*unload_, WARM)
                                 : *unload_);
        touched_.notify_all();
    }

    // One thread stands in for a timer per use: only the latest use's timer could ever fire.
    void unload_when_idle(std::stop_token stop) {
        std::unique_lock lock(unload_mutex_);
        while (!stop.stop_requested()) {
            auto use = uses_;
            auto touched = [&] { return uses_ != use; };
            if (!deadline_) {
                touched_.wait(lock, stop, touched);
                continue;
            }
            // A copy: the wait reads the deadline while touch() may replace it.
            auto deadline = *deadline_;
            if (touched_.wait_until(lock, stop, deadline, touched) || stop.stop_requested())
                continue;
            deadline_.reset();
            std::shared_ptr<Server> unloaded;
            {
                std::lock_guard guard(server_mutex_);
                unloaded = std::move(server_);
            }
            lock.unlock();
            unloaded.reset();
            lock.lock();
        }
    }

    Plan plan() const {
        const auto& runtime = s1::runtime();
        return {&runtime, dir_ / runtime.name, dir_ / MODEL.file()};
    }

    std::shared_ptr<Server> running() {
        std::shared_ptr<Server> dead;
        std::lock_guard lock(server_mutex_);
        if (server_ && !server_->child->alive()) dead = std::exchange(server_, nullptr);
        return server_;
    }

    std::shared_ptr<Server> server(std::stop_token stop) {
        if (auto server = running()) return server;
        std::unique_lock starting(starting_, std::defer_lock);
        while (!starting.try_lock_for(std::chrono::milliseconds(100)))
            if (stop.stop_requested()) throw Error("Cancelled");
        if (auto server = running()) return server;
        check();
        auto plan = this->plan();
        bool gpu = plan.runtime->gpu && !cpu_only_;
        std::shared_ptr<Server> server;
        try {
            server = spawn(plan.directory, plan.model, gpu, stop);
        } catch (...) {
            // A GPU backend that cannot start falls back to the CPU.
            if (!gpu || stop.stop_requested()) throw;
            cpu_only_ = true;
            server = spawn(plan.directory, plan.model, false, stop);
        }
        {
            std::lock_guard lock(server_mutex_);
            server_ = server;
        }
        // Never left loaded if the call that started it is abandoned.
        touch(true);
        return server;
    }

    std::shared_ptr<Server> spawn(const std::filesystem::path& directory,
                                  const std::filesystem::path& model, bool gpu,
                                  std::stop_token stop) {
        auto port = std::to_string(free_port());
        auto server = std::make_shared<Server>();
        server->url = "http://127.0.0.1:" + port;
        server->key = uuid_v4();
        std::vector<std::string> args = {"--host",     "127.0.0.1", "--port",   port,
                                         "-c",         "4096",      "-np",      "1",
                                         "--no-webui", "-m",        utf8(model)};
        if (gpu)
            args.insert(args.end(), {"-ngl", "99", "-sm", "none"});
        else
            args.insert(args.end(), {"--device", "none"});
        try {
            server->child = std::make_unique<Process>(
                directory / SERVER, args, directory,
                std::vector<std::pair<std::string, std::string>>{{"LLAMA_API_KEY", server->key}},
                dir_ / "llama-server.log");
        } catch (const std::exception&) {
            throw Error(START_FAILED);
        }
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(180);
        for (;;) {
            if (!server->child->alive() || std::chrono::steady_clock::now() > deadline)
                throw Error(START_FAILED);
            try {
                auto status = http::perform(
                    local(server->url + "/health"), [](long, std::string_view) { return true; },
                    stop);
                if (status >= 200 && status <= 299) return server;
            } catch (const http::Failure&) {
            }
            if (!sleep_for(stop, std::chrono::milliseconds(100))) throw Error("Cancelled");
        }
    }

    void download_part(Part part, std::stop_token cancel) {
        auto plan = this->plan();
        auto missing = plan.missing(part);
        std::uint64_t total = 0;
        for (const auto* asset : missing)
            total += asset->size;
        if (total == 0) return;
        modify(
            [&](Status& status) {
                slot(status, part).progress = 0.;
                slot(status, part).error.reset();
            },
            true);
        std::filesystem::create_directories(dir_);
        std::uint64_t done = 0;
        for (const auto* asset : missing) {
            fetch(*asset, dir_ / asset->file(), cancel, [&](std::uint64_t bytes) {
                done += bytes;
                auto progress =
                    std::floor(static_cast<double>(done) / static_cast<double>(total) * 1000) /
                    1000;
                modify([&](Status& status) { slot(status, part).progress = progress; }, false);
            });
        }
        if (part == Part::Engine) {
            const auto& runtime = *plan.runtime;
            std::vector<std::filesystem::path> archives;
            for (const auto& asset : runtime.assets)
                archives.push_back(dir_ / asset.file());
            install_runtime(archives, dir_ / (runtime.name + ".part"), dir_ / runtime.name);
            for (const auto& archive : archives)
                std::filesystem::remove(archive);
            // Only one llama.cpp build is kept.
            for (const auto& entry : std::filesystem::directory_iterator(dir_)) {
                auto name = entry.path().filename().string();
                if (name.starts_with("llama-") && name != runtime.name &&
                    std::filesystem::is_directory(entry.symlink_status()))
                    std::filesystem::remove_all(entry.path());
            }
        }
    }
};

std::shared_ptr<Engine> Engine::create(std::filesystem::path dir) {
    return std::shared_ptr<Engine>(new Engine(std::make_shared<State>(std::move(dir))));
}
Engine::Engine(std::shared_ptr<State> state) : state_(std::move(state)) {}
Engine::~Engine() = default;
void Engine::on_status(std::function<void(const Status&)> listener) {
    std::lock_guard lock(state_->listeners_mutex_);
    state_->listeners_.push_back(std::move(listener));
}
Status Engine::status() const {
    return state_->status();
}
void Engine::check() const {
    state_->check();
}
void Engine::download(Part part) {
    state_->download(part);
}
void Engine::cancel_download(Part part) {
    state_->cancel_download(part);
}
void Engine::stop() {
    state_->stop();
}
void Engine::set_unload(std::optional<std::chrono::seconds> unload) {
    state_->set_unload(unload);
}
void Engine::warm() {
    state_->server({});
    state_->touch(true);
}
std::string Engine::clean(const std::string& transcript, Styling styling, std::stop_token stop) {
    return state_->clean(transcript, styling, stop);
}

} // namespace transcribe::s1
