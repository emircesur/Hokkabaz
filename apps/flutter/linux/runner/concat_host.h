#pragma once
#include <flutter_linux/flutter_linux.h>
#include <gtk/gtk.h>

// The full editor is an X11 child, not a screenshot or a reduced Flutter port.
void concat_host_register(FlView* view, GtkWindow* window, GtkOverlay* overlay);
