#include "s1/engine.hpp"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <spawn.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#include <system_error>

extern char** environ;

namespace transcribe::s1 {

const Runtime& runtime() {
#ifdef __x86_64__
    static const Runtime VULKAN{
        "llama-b11153-vulkan",
        {llama("llama-b11153-bin-ubuntu-vulkan-x64.tar.gz",
               "5cbe209c19456e94d300cc41a0cd4eff1d367404736135754b031f29634a76de", 30'598'114)},
        true};
    return VULKAN;
#else
    throw Error("S1-mini unavailable on this platform");
#endif
}

std::uint16_t free_port() {
    int s = ::socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (s < 0) throw Error(START_FAILED);
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    socklen_t size = sizeof address;
    bool bound = ::bind(s, reinterpret_cast<sockaddr*>(&address), sizeof address) == 0 &&
                 ::getsockname(s, reinterpret_cast<sockaddr*>(&address), &size) == 0;
    ::close(s);
    if (!bound) throw Error(START_FAILED);
    return ntohs(address.sin_port);
}

// Like Rust's Command: the child gets an empty signal mask and the default SIGPIPE action. It is
// killed when dropped, as with tokio's kill_on_drop, not when the app dies.
Process::Process(const std::filesystem::path& program, const std::vector<std::string>& args,
                 const std::filesystem::path& directory,
                 const std::vector<std::pair<std::string, std::string>>& environment,
                 const std::filesystem::path& log) {
    int output = ::open(log.c_str(), O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0666);
    if (output < 0) throw std::system_error(errno, std::generic_category());
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
    posix_spawn_file_actions_adddup2(&actions, output, 1);
    posix_spawn_file_actions_adddup2(&actions, output, 2);
    posix_spawn_file_actions_addchdir_np(&actions, directory.c_str());
    posix_spawnattr_t attributes;
    posix_spawnattr_init(&attributes);
    sigset_t signals;
    sigemptyset(&signals);
    posix_spawnattr_setsigmask(&attributes, &signals);
    sigaddset(&signals, SIGPIPE);
    posix_spawnattr_setsigdefault(&attributes, &signals);
    posix_spawnattr_setflags(&attributes, POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF);

    std::vector<char*> argv{const_cast<char*>(program.c_str())};
    for (const auto& arg : args)
        argv.push_back(const_cast<char*>(arg.c_str()));
    argv.push_back(nullptr);
    std::vector<std::string> variables;
    for (char** entry = environ; *entry; ++entry) {
        std::string_view variable(*entry);
        bool replaced = false;
        for (const auto& [name, value] : environment)
            replaced = replaced || variable.starts_with(name + "=");
        if (!replaced) variables.emplace_back(variable);
    }
    for (const auto& [name, value] : environment)
        variables.push_back(name + "=" + value);
    std::vector<char*> envp;
    for (auto& variable : variables)
        envp.push_back(variable.data());
    envp.push_back(nullptr);

    int error =
        posix_spawnp(&pid_, program.c_str(), &actions, &attributes, argv.data(), envp.data());
    posix_spawnattr_destroy(&attributes);
    posix_spawn_file_actions_destroy(&actions);
    ::close(output);
    if (error != 0) throw std::system_error(error, std::generic_category());
}

Process::~Process() {
    if (exited_) return;
    ::kill(pid_, SIGKILL);
    ::waitpid(pid_, nullptr, 0);
}

bool Process::alive() {
    if (exited_) return false;
    int status;
    if (::waitpid(pid_, &status, WNOHANG) == 0) return true;
    exited_ = true;
    return false;
}

} // namespace transcribe::s1
