#include "concat_host.h"

#include <flutter/standard_method_codec.h>
#include <algorithm>
#include <vector>

namespace {
using Value = flutter::EncodableValue;
using Map = flutter::EncodableMap;
constexpr UINT_PTR kTimer = 0x484B;
std::wstring Wide(const std::string& value) {
  int size = MultiByteToWideChar(CP_UTF8, 0, value.c_str(), -1, nullptr, 0);
  if (size <= 0) return {};
  std::vector<wchar_t> result(static_cast<size_t>(size));
  MultiByteToWideChar(CP_UTF8, 0, value.c_str(), -1, result.data(), size);
  return result.data();
}
std::string String(const Map& args, const char* key) {
  auto it = args.find(Value(key));
  if (it == args.end()) return {};
  auto value = std::get_if<std::string>(&it->second);
  return value ? *value : std::string();
}
double Number(const Map& args, const char* key) {
  auto it = args.find(Value(key));
  if (it == args.end()) return 0;
  if (auto value = std::get_if<double>(&it->second)) return *value;
  if (auto value = std::get_if<int32_t>(&it->second)) return *value;
  return 0;
}
struct Search { DWORD pid; HWND window = nullptr; };
BOOL CALLBACK FindEditor(HWND window, LPARAM data) {
  auto* search = reinterpret_cast<Search*>(data);
  DWORD pid = 0;
  GetWindowThreadProcessId(window, &pid);
  // Ignore hidden helper surfaces and owned file-picker/message-box windows.
  if (pid == search->pid && GetWindowTextLengthW(window) > 0 && !GetWindow(window, GW_OWNER)) {
    search->window = window;
    return FALSE;
  }
  return TRUE;
}
// Windows paths cannot contain a double quote. Always quote both argv entries;
// never send a project path through cmd.exe or a shell.
std::wstring Quote(const std::wstring& value) {
  std::wstring out = L"\"";
  size_t slashes = 0;
  for (wchar_t c : value) {
    if (c == L'\\') { ++slashes; continue; }
    out.append(slashes * (c == L'"' ? 2 : 1), L'\\');
    if (c == L'"') out += L'\\';
    out += c;
    slashes = 0;
  }
  out.append(slashes * 2, L'\\');
  return out + L'"';
}
}

ConcatHost::ConcatHost(HWND parent, flutter::BinaryMessenger* messenger) : parent_(parent) {
  channel_ = std::make_unique<flutter::MethodChannel<Value>>(
      messenger, "hokkabaz/concat_editor", &flutter::StandardMethodCodec::GetInstance());
  channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    const auto* args = call.arguments() ? std::get_if<Map>(call.arguments()) : nullptr;
    const Map empty;
    const auto& values = args ? *args : empty;
    const auto& method = call.method_name();
    if (method == "start") {
      Poll();
      const auto binary = Wide(String(values, "binary"));
      const auto project = Wide(String(values, "path"));
      if (process_.hProcess && project != project_) {
        result->Error("editor_busy", "Save and close the current editor before opening another project.");
        return;
      }
      if (!process_.hProcess) {
        if (binary.empty()) {
          result->Error("invalid_path", "Editor executable is required."); return;
        }
        auto command = Quote(binary) + (project.empty() ? L"" : L" --project " + Quote(project));
        STARTUPINFOW startup{};
        startup.cb = sizeof(startup);
        startup.dwFlags = STARTF_USESHOWWINDOW;
        startup.wShowWindow = SW_HIDE;
        if (!CreateProcessW(binary.c_str(), command.data(), nullptr, nullptr, FALSE,
                            CREATE_UNICODE_ENVIRONMENT, nullptr, nullptr, &startup, &process_)) {
          result->Error("launch_failed", "Cannot start bundled Concat (Win32 " + std::to_string(GetLastError()) + ")."); return;
        }
        project_ = project;
        error_.clear();
        attempts_ = 0;
        SetTimer(parent_, kTimer, 100, nullptr);
      }
      visible_ = true;
      Layout();
      result->Success();
    } else if (method == "bounds") {
      x_ = Number(values, "x"); y_ = Number(values, "y");
      width_ = Number(values, "width"); height_ = Number(values, "height");
      Layout(); result->Success();
    } else if (method == "visible") {
      auto it = values.find(Value("visible"));
      visible_ = it != values.end() && std::get_if<bool>(&it->second) && std::get<bool>(it->second);
      Layout(); result->Success();
    } else if (method == "status") {
      Poll();
      result->Success(Value(Map{{Value("running"), Value(process_.hProcess != nullptr)},
        {Value("embedded"), Value(editor_ != nullptr)}, {Value("error"), Value(error_)}}));
    } else if (method == "save") {
      Save(); result->Success();
    } else if (method == "close") {
      Close(); result->Success();
    } else { result->NotImplemented(); }
  });
}

ConcatHost::~ConcatHost() {
  KillTimer(parent_, kTimer);
  if (channel_) channel_->SetMethodCallHandler(nullptr);
  // Normal WM_CLOSE waits for the editor (including its save prompt). Never
  // terminate it on route changes or pretend a synthetic save was acknowledged.
  if (editor_) PostMessageW(editor_, WM_CLOSE, 0, 0);
  if (process_.hThread) CloseHandle(process_.hThread);
  if (process_.hProcess) CloseHandle(process_.hProcess);
}

void ConcatHost::Poll() {
  if (!process_.hProcess) return;
  if (WaitForSingleObject(process_.hProcess, 0) == WAIT_OBJECT_0) {
    DWORD exit = 0;
    GetExitCodeProcess(process_.hProcess, &exit);
    if (!editor_) error_ = "Concat exited before its editor opened (" + std::to_string(exit) + ").";
    CloseHandle(process_.hThread); CloseHandle(process_.hProcess);
    process_ = {}; editor_ = nullptr;
    KillTimer(parent_, kTimer);
    if (closing_parent_) PostMessageW(parent_, WM_CLOSE, 0, 0);
    return;
  }
  if (!editor_) {
    Search search{process_.dwProcessId};
    EnumWindows(FindEditor, reinterpret_cast<LPARAM>(&search));
    if (search.window) {
      auto window = search.window;
      ShowWindow(window, SW_HIDE);
      auto style = GetWindowLongPtrW(window, GWL_STYLE);
      SetWindowLongPtrW(window, GWL_STYLE, (style & ~(WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU)) | WS_CHILD | WS_CLIPSIBLINGS);
      SetWindowLongPtrW(window, GWL_EXSTYLE, GetWindowLongPtrW(window, GWL_EXSTYLE) & ~WS_EX_APPWINDOW);
      SetLastError(0);
      if (!SetParent(window, parent_) && GetLastError() != 0) {
        SetWindowLongPtrW(window, GWL_STYLE, style);
        error_ = "Windows refused the editor surface (check DPI compatibility).";
        PostMessageW(window, WM_CLOSE, 0, 0);
        return;
      }
      editor_ = window;
      SetWindowPos(window, nullptr, 0, 0, 1, 1, SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
      Layout();
    } else if (++attempts_ == 300) {
      error_ = "Concat is still starting; check its startup log or graphics driver.";
    }
  }
}

void ConcatHost::Layout() {
  if (!editor_ || !IsWindow(editor_)) return;
  const double scale = GetDpiForWindow(parent_) / 96.0;
  SetWindowPos(editor_, HWND_TOP, static_cast<int>(x_ * scale), static_cast<int>(y_ * scale),
    std::max(1, static_cast<int>(width_ * scale)), std::max(1, static_cast<int>(height_ * scale)), SWP_NOACTIVATE);
  ShowWindow(editor_, visible_ && width_ > 0 && height_ > 0 ? SW_SHOWNA : SW_HIDE);
}

void ConcatHost::Save() {
  if (!editor_) return;
  SetForegroundWindow(parent_);
  DWORD own = GetCurrentThreadId(), other = GetWindowThreadProcessId(editor_, nullptr);
  if (own != other) AttachThreadInput(own, other, TRUE);
  SetFocus(editor_);
  INPUT keys[4]{};
  for (auto& key : keys) key.type = INPUT_KEYBOARD;
  keys[0].ki.wVk = VK_CONTROL; keys[1].ki.wVk = 'S';
  keys[2].ki.wVk = 'S'; keys[2].ki.dwFlags = KEYEVENTF_KEYUP;
  keys[3].ki.wVk = VK_CONTROL; keys[3].ki.dwFlags = KEYEVENTF_KEYUP;
  SendInput(4, keys, sizeof(INPUT));
  if (own != other) AttachThreadInput(own, other, FALSE);
}

void ConcatHost::Close() {
  if (editor_) { visible_ = true; Layout(); PostMessageW(editor_, WM_CLOSE, 0, 0); }
}

bool ConcatHost::RequestClose() {
  Poll();
  if (!process_.hProcess) return false;
  if (!editor_) TerminateProcess(process_.hProcess, 0); // No editor has been exposed yet.
  closing_parent_ = true;
  Close();
  return true;
}

bool ConcatHost::HandleMessage(UINT message) {
  if (message == WM_TIMER) Poll();
  if (message == WM_SIZE || message == WM_DPICHANGED) Layout();
  return false;
}
