#include "concat_host.h"

#include <gdk/gdkx.h>
#include <gtk/gtkx.h>
#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <sys/wait.h>

#include <string>
#include <algorithm>

namespace {
struct Host {
  GtkWindow* window;
  GtkWidget* fixed;
  GtkWidget* socket;
  FlMethodChannel* channel = nullptr;
  GPid pid = 0;
  Window child = 0;
  guint timer = 0;
  std::string project, error;
  bool visible = false, closing_parent = false;
  int attempts = 0;

  Display* display() { return GDK_DISPLAY_XDISPLAY(gtk_widget_get_display(socket)); }
  void close() {
    if (!child) return;
    visible = true;
    gtk_widget_show(socket);
    XEvent event{};
    event.xclient.type = ClientMessage;
    event.xclient.window = child;
    event.xclient.message_type = XInternAtom(display(), "WM_PROTOCOLS", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = static_cast<long>(XInternAtom(display(), "WM_DELETE_WINDOW", False));
    event.xclient.data.l[1] = CurrentTime;
    XSendEvent(display(), child, False, NoEventMask, &event);
    XFlush(display());
  }
  void save() {
    if (!child) return;
    XSetInputFocus(display(), child, RevertToParent, CurrentTime);
    XKeyEvent key{};
    key.display = display(); key.window = child; key.root = DefaultRootWindow(display());
    key.same_screen = True; key.state = ControlMask;
    key.keycode = XKeysymToKeycode(display(), XK_s);
    key.type = KeyPress;
    XSendEvent(display(), child, True, KeyPressMask, reinterpret_cast<XEvent*>(&key));
    key.type = KeyRelease;
    XSendEvent(display(), child, True, KeyReleaseMask, reinterpret_cast<XEvent*>(&key));
    XFlush(display());
  }
  Window find(Window root) {
    Window returned, parent, *children = nullptr;
    unsigned count = 0;
    if (!XQueryTree(display(), root, &returned, &parent, &children, &count)) return 0;
    Window found = 0;
    Atom atom = XInternAtom(display(), "_NET_WM_PID", False);
    for (unsigned i = 0; i < count && !found; ++i) {
      Atom type; int format; unsigned long size, after; unsigned char* data = nullptr;
      if (XGetWindowProperty(display(), children[i], atom, 0, 1, False, AnyPropertyType,
          &type, &format, &size, &after, &data) == Success && data) {
        if (format == 32 && size == 1 && *reinterpret_cast<unsigned long*>(data) == static_cast<unsigned long>(pid)) found = children[i];
        XFree(data);
      }
      if (!found) found = find(children[i]);
    }
    if (children) XFree(children);
    return found;
  }
  void poll() {
    if (!pid || child) return;
    auto* gdisplay = gtk_widget_get_display(socket);
    gdk_x11_display_error_trap_push(gdisplay);
    Window found = find(DefaultRootWindow(display()));
    if (found) {
      gtk_socket_add_id(GTK_SOCKET(socket), found);
      child = found;
      if (!visible) gtk_widget_hide(socket);
      error.clear();
    } else if (++attempts == 300) {
      error = "Concat is still starting; check its graphics/startup log.";
    }
    gdk_x11_display_error_trap_pop_ignored(gdisplay);
  }
  ~Host() {
    if (timer) g_source_remove(timer);
    if (channel) { fl_method_channel_set_method_call_handler(channel, nullptr, nullptr, nullptr); g_object_unref(channel); }
  }
};

const char* string(FlValue* args, const char* name) {
  FlValue* value = args ? fl_value_lookup_string(args, name) : nullptr;
  return value && fl_value_get_type(value) == FL_VALUE_TYPE_STRING ? fl_value_get_string(value) : "";
}
double number(FlValue* args, const char* name) {
  FlValue* value = args ? fl_value_lookup_string(args, name) : nullptr;
  if (!value) return 0;
  return fl_value_get_type(value) == FL_VALUE_TYPE_FLOAT ? fl_value_get_float(value) :
    fl_value_get_type(value) == FL_VALUE_TYPE_INT ? static_cast<double>(fl_value_get_int(value)) : 0;
}
void call(FlMethodChannel*, FlMethodCall* method, gpointer data) {
  auto* host = static_cast<Host*>(data);
  const char* name = fl_method_call_get_name(method);
  FlValue* args = fl_method_call_get_args(method);
  const bool x11 = GDK_IS_X11_DISPLAY(gtk_widget_get_display(host->socket));
  if (!x11) {
    fl_method_call_respond_error(method, "requires_x11", "Embedded Concat needs an X11 session (or XWayland with GDK_BACKEND=x11).", nullptr, nullptr); return;
  }
  if (g_str_equal(name, "start")) {
    const char* path = string(args, "path");
    if (host->pid && host->project != path) {
      fl_method_call_respond_error(method, "editor_busy", "Save and close the current editor before opening another project.", nullptr, nullptr); return;
    }
    if (!host->pid) {
      const char* binary = string(args, "binary");
      gchar* argv[] = {const_cast<gchar*>(binary), const_cast<gchar*>("--project"), const_cast<gchar*>(path), nullptr};
      g_auto(GStrv) env = g_get_environ();
      env = g_environ_setenv(env, "WINIT_UNIX_BACKEND", "x11", TRUE);
      env = g_environ_setenv(env, "SLINT_BACKEND", "winit", TRUE);
      env = g_environ_unsetenv(env, "WAYLAND_DISPLAY");
      g_autoptr(GError) error = nullptr;
      if (!g_spawn_async(nullptr, argv, env, static_cast<GSpawnFlags>(G_SPAWN_DO_NOT_REAP_CHILD | G_SPAWN_SEARCH_PATH), nullptr, nullptr, &host->pid, &error)) {
        fl_method_call_respond_error(method, "launch_failed", error->message, nullptr, nullptr); return;
      }
      host->project = path; host->error.clear(); host->attempts = 0;
      g_child_watch_add(host->pid, [](GPid pid, gint status, gpointer data) {
        auto* h = static_cast<Host*>(data);
        if (!h->child) h->error = "Concat exited before its editor opened (" + std::to_string(status) + ").";
        h->pid = 0; h->child = 0;
        gtk_widget_hide(h->socket);
        g_spawn_close_pid(pid);
        if (h->closing_parent) gtk_window_close(h->window);
      }, host);
    }
    host->visible = true;
    gtk_widget_show(host->socket);
  } else if (g_str_equal(name, "bounds")) {
    gtk_fixed_move(GTK_FIXED(host->fixed), host->socket, static_cast<int>(number(args, "x")), static_cast<int>(number(args, "y")));
    gtk_widget_set_size_request(host->socket, std::max(1, static_cast<int>(number(args, "width"))), std::max(1, static_cast<int>(number(args, "height"))));
  } else if (g_str_equal(name, "visible")) {
    auto* value = fl_value_lookup_string(args, "visible");
    host->visible = value && fl_value_get_type(value) == FL_VALUE_TYPE_BOOL && fl_value_get_bool(value);
    if (host->visible) gtk_widget_show(host->socket); else gtk_widget_hide(host->socket);
  } else if (g_str_equal(name, "status")) {
    host->poll();
    g_autoptr(FlValue) status = fl_value_new_map();
    fl_value_set_string_take(status, "running", fl_value_new_bool(host->pid != 0));
    fl_value_set_string_take(status, "embedded", fl_value_new_bool(host->child != 0));
    fl_value_set_string_take(status, "error", fl_value_new_string(host->error.c_str()));
    fl_method_call_respond_success(method, status, nullptr); return;
  } else if (g_str_equal(name, "save")) { host->save();
  } else if (g_str_equal(name, "close")) { host->close();
  } else { fl_method_call_respond_not_implemented(method, nullptr); return; }
  fl_method_call_respond_success(method, nullptr, nullptr);
}
}

void concat_host_register(FlView* view, GtkWindow* window, GtkOverlay* overlay) {
  auto* host = new Host{};
  host->window = window;
  host->fixed = gtk_fixed_new();
  gtk_widget_set_halign(host->fixed, GTK_ALIGN_FILL);
  gtk_widget_set_valign(host->fixed, GTK_ALIGN_FILL);
  gtk_overlay_add_overlay(overlay, host->fixed);
  gtk_overlay_set_overlay_pass_through(overlay, host->fixed, TRUE);
  gtk_widget_show(host->fixed);
  host->socket = gtk_socket_new();
  gtk_fixed_put(GTK_FIXED(host->fixed), host->socket, 0, 0);
  gtk_widget_realize(host->socket);
  g_signal_connect(host->socket, "plug-removed", G_CALLBACK(+[](GtkSocket*, gpointer data) -> gboolean {
    static_cast<Host*>(data)->child = 0;
    return TRUE; // Retain the socket for the next project.
  }), host);
  g_signal_connect(window, "delete-event", G_CALLBACK(+[](GtkWidget*, GdkEvent*, gpointer data) -> gboolean {
    auto* h = static_cast<Host*>(data);
    if (!h->pid) return FALSE;
    h->closing_parent = true;
    h->close();
    return TRUE;
  }), host);
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  host->channel = fl_method_channel_new(fl_engine_get_binary_messenger(fl_view_get_engine(view)), "hokkabaz/concat_editor", FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(host->channel, call, host, nullptr);
  host->timer = g_timeout_add(100, [](gpointer data) -> gboolean { static_cast<Host*>(data)->poll(); return G_SOURCE_CONTINUE; }, host);
  // GtkApplication holds this window until the child has handled its save prompt.
  g_object_set_data_full(G_OBJECT(window), "concat-host", host, [](gpointer data) { delete static_cast<Host*>(data); });
}
