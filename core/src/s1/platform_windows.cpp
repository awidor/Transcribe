#include "s1/engine.hpp"

#include <windows.h>
#include <winsock2.h>
#include <ws2tcpip.h>

#include <optional>
#include <system_error>

namespace transcribe::s1 {

namespace {

// The NVIDIA driver's CUDA version and the first GPU's compute capability.
std::optional<std::pair<int, int>> cuda() {
    using Init = int(WINAPI*)(unsigned);
    using Version = int(WINAPI*)(int*);
    using Device = int(WINAPI*)(int*, int);
    using Attribute = int(WINAPI*)(int*, int, int);
    constexpr int MAJOR = 75, MINOR = 76;
    // nvcuda.dll ships with every NVIDIA driver and is only loaded from System32.
    auto library = LoadLibraryExA("nvcuda.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!library) return std::nullopt;
    auto symbol = [&](const char* name) {
        return reinterpret_cast<void*>(GetProcAddress(library, name));
    };
    auto init = reinterpret_cast<Init>(symbol("cuInit"));
    auto version = reinterpret_cast<Version>(symbol("cuDriverGetVersion"));
    auto device = reinterpret_cast<Device>(symbol("cuDeviceGet"));
    auto attribute = reinterpret_cast<Attribute>(symbol("cuDeviceGetAttribute"));
    if (!init || !version || !device || !attribute) return std::nullopt;
    int driver = 0, ordinal = 0, major = 0, minor = 0;
    if (init(0) == 0 && version(&driver) == 0 && device(&ordinal, 0) == 0 &&
        attribute(&major, MAJOR, ordinal) == 0 && attribute(&minor, MINOR, ordinal) == 0)
        return std::pair{driver, major * 10 + minor};
    return std::nullopt;
}

std::wstring widen(const std::string& text) {
    if (text.empty()) return {};
    int size =
        MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0);
    std::wstring wide(static_cast<std::size_t>(size), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), wide.data(), size);
    return wide;
}

// Quotes an argument the way CommandLineToArgvW reads it back.
std::wstring quote(const std::wstring& arg) {
    if (!arg.empty() && arg.find_first_of(L" \t\n\v\"") == std::wstring::npos) return arg;
    std::wstring quoted = L"\"";
    std::size_t backslashes = 0;
    for (auto c : arg) {
        if (c == L'\\') {
            ++backslashes;
            continue;
        }
        quoted.append(c == L'"' ? backslashes * 2 + 1 : backslashes, L'\\');
        quoted += c;
        backslashes = 0;
    }
    quoted.append(backslashes * 2, L'\\');
    return quoted + L"\"";
}

// A job object that closes with this process takes the server down with it, even when the app
// crashes.
void contain(HANDLE process) {
    static HANDLE job = [] {
        HANDLE created = CreateJobObjectW(nullptr, nullptr);
        if (!created) return created;
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(created, JobObjectExtendedLimitInformation, &limits, sizeof limits);
        return created;
    }();
    if (job) AssignProcessToJobObject(job, process);
}

} // namespace

const Runtime& runtime() {
#if defined(_M_X64) || defined(__x86_64__)
    static const Runtime CUDA_13{
        "llama-b11153-cuda-13",
        {llama("llama-b11153-bin-win-cuda-13.4-x64.zip",
               "7bef2d6a1594aedf61f011bb04051ec0f2037d951b87197ba5f3411544c99a10", 149'758'635),
         llama("cudart-llama-bin-win-cuda-13.4-x64.zip",
               "738f8c251ac22b70c3ae6f83a10cf222725df0395246a2cf58f32bdb85fbe668", 423'535'356)},
        true};
    static const Runtime CUDA_12{
        "llama-b11153-cuda-12",
        {llama("llama-b11153-bin-win-cuda-12.4-x64.zip",
               "0d5c737b6c5ba61f971515e1e008a52073e606914b4ac4ac8d6f27389b84935a", 253'869'597),
         llama("cudart-llama-bin-win-cuda-12.4-x64.zip",
               "8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6", 391'443'627)},
        true};
    // Vulkan covers AMD and Intel GPUs, and runs on the CPU when there is none.
    static const Runtime VULKAN{
        "llama-b11153-vulkan",
        {llama("llama-b11153-bin-win-vulkan-x64.zip",
               "68925200a1a7543adb83c5dabb4611f4e205e6af10f1b13b7f61604a105bc892", 32'126'804)},
        true};
    static const Runtime& detected = []() -> const Runtime& {
        auto found = cuda();
        if (found && found->first >= 13000 && found->second >= 75) return CUDA_13;
        if (found && found->first >= 12000 && found->second >= 50) return CUDA_12;
        return VULKAN;
    }();
    return detected;
#else
    throw Error("S1-mini unavailable on this platform");
#endif
}

std::uint16_t free_port() {
    static const bool started = [] {
        WSADATA data;
        return WSAStartup(MAKEWORD(2, 2), &data) == 0;
    }();
    SOCKET s = started ? ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP) : INVALID_SOCKET;
    if (s == INVALID_SOCKET) throw Error(START_FAILED);
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int size = sizeof address;
    bool bound = ::bind(s, reinterpret_cast<sockaddr*>(&address), sizeof address) == 0 &&
                 ::getsockname(s, reinterpret_cast<sockaddr*>(&address), &size) == 0;
    ::closesocket(s);
    if (!bound) throw Error(START_FAILED);
    return ntohs(address.sin_port);
}

Process::Process(const std::filesystem::path& program, const std::vector<std::string>& args,
                 const std::filesystem::path& directory,
                 const std::vector<std::pair<std::string, std::string>>& environment,
                 const std::filesystem::path& log) {
    SECURITY_ATTRIBUTES inherit{sizeof(SECURITY_ATTRIBUTES), nullptr, TRUE};
    HANDLE output = CreateFileW(log.c_str(), GENERIC_WRITE,
                                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, &inherit,
                                CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (output == INVALID_HANDLE_VALUE)
        throw std::system_error(static_cast<int>(GetLastError()), std::system_category());
    HANDLE input = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &inherit,
                               OPEN_EXISTING, 0, nullptr);
    STARTUPINFOW startup{};
    startup.cb = sizeof startup;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = input;
    startup.hStdOutput = output;
    startup.hStdError = output;

    auto command = quote(program.wstring());
    for (const auto& arg : args)
        command += L" " + quote(widen(arg));
    std::wstring block;
    auto* strings = GetEnvironmentStringsW();
    for (auto* entry = strings; *entry; entry += wcslen(entry) + 1) {
        std::wstring_view variable(entry);
        bool replaced = false;
        for (const auto& [name, value] : environment) {
            auto wide = widen(name);
            replaced =
                replaced || (variable.size() > wide.size() && variable[wide.size()] == L'=' &&
                             _wcsnicmp(entry, wide.c_str(), wide.size()) == 0);
        }
        if (!replaced) block.append(variable).push_back(L'\0');
    }
    FreeEnvironmentStringsW(strings);
    for (const auto& [name, value] : environment)
        block.append(widen(name + "=" + value)).push_back(L'\0');
    block.push_back(L'\0');

    PROCESS_INFORMATION info{};
    BOOL started = CreateProcessW(nullptr, command.data(), nullptr, nullptr, TRUE,
                                  CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT, block.data(),
                                  directory.c_str(), &startup, &info);
    auto error = GetLastError();
    CloseHandle(output);
    if (input != INVALID_HANDLE_VALUE) CloseHandle(input);
    if (!started) throw std::system_error(static_cast<int>(error), std::system_category());
    CloseHandle(info.hThread);
    handle_ = info.hProcess;
    contain(info.hProcess);
}

Process::~Process() {
    TerminateProcess(handle_, 1);
    CloseHandle(handle_);
}

bool Process::alive() {
    return WaitForSingleObject(handle_, 0) == WAIT_TIMEOUT;
}

} // namespace transcribe::s1
