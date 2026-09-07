declare global {
  interface Window {
    __TRANSCRIBE_PLATFORM__?: string;
  }
}

// The desktop supplies its compile target before React loads. The fallback is
// for browser previews; it does not decide native behavior or permissions.
export const platform =
  window.__TRANSCRIBE_PLATFORM__ ||
  (/Windows/i.test(navigator.userAgent)
    ? 'windows'
    : /Mac/i.test(navigator.userAgent)
      ? 'macos'
      : 'linux');
document.documentElement.dataset.platform = platform;
