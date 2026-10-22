#ifndef RUNNER_CONCAT_HOST_H_
#define RUNNER_CONCAT_HOST_H_

#include <flutter/binary_messenger.h>
#include <flutter/encodable_value.h>
#include <flutter/method_channel.h>
#include <windows.h>

#include <memory>
#include <string>

// Hosts the unmodified upstream editor as a child surface. Its menus, timeline,
// GPU preview, shortcuts and native dialogs remain the upstream implementation.
class ConcatHost {
 public:
  ConcatHost(HWND parent, flutter::BinaryMessenger* messenger);
  ~ConcatHost();
  bool HandleMessage(UINT message);
  bool RequestClose();

 private:
  void Poll();
  void Layout();
  void Close();
  void Save();
  HWND parent_;
  HWND editor_ = nullptr;
  PROCESS_INFORMATION process_{};
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> channel_;
  std::wstring project_;
  bool visible_ = false;
  bool closing_parent_ = false;
  unsigned attempts_ = 0;
  double x_ = 0, y_ = 0, width_ = 1, height_ = 1;
  std::string error_;
};
#endif
