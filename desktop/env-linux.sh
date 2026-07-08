# Source before `npm run tauri dev|build` when system GTK/WebKit -dev packages
# are not installed (user-local sysroot under ~/.local/tauri-sysroot).
PREFIX="${TAURI_SYSROOT:-$HOME/.local/tauri-sysroot}"
export PKG_CONFIG_SYSROOT_DIR="$PREFIX"
export PKG_CONFIG_PATH="$PREFIX/usr/lib/x86_64-linux-gnu/pkgconfig:$PREFIX/usr/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export PKG_CONFIG_LIBDIR="$PREFIX/usr/lib/x86_64-linux-gnu/pkgconfig:$PREFIX/usr/share/pkgconfig"
export LIBRARY_PATH="$PREFIX/usr/lib/x86_64-linux-gnu:$PREFIX/usr/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$PREFIX/usr/lib/x86_64-linux-gnu:$PREFIX/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export CPATH="$PREFIX/usr/include${CPATH:+:$CPATH}"
export C_INCLUDE_PATH="$PREFIX/usr/include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
export CPLUS_INCLUDE_PATH="$PREFIX/usr/include${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}"
export PATH="$PREFIX/usr/bin:$PATH"
