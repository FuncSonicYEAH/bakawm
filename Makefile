PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
LIBDIR ?= $(PREFIX)/lib
SYSCONFDIR ?= $(PREFIX)/etc

SYSTEMD_USER_UNIT_DIR ?= $(LIBDIR)/systemd/user
DINIT_USER_DIR ?= $(LIBDIR)/dinit.d/user
WAYLAND_SESSIONS_DIR ?= $(DATADIR)/wayland-sessions
PORTAL_DIR ?= $(DATADIR)/xdg-desktop-portal

FEATURES ?=

CARGO ?= cargo
CARGO_FLAGS ?= --release --locked

SESSION_MODE ?= auto

ifeq ($(SESSION_MODE),auto)
_HAS_SYSTEMD := $(shell command -v systemctl >/dev/null 2>&1 && echo yes)
_HAS_DINIT := $(shell command -v dinitctl >/dev/null 2>&1 && echo yes)
ifneq ($(filter yes,$(_HAS_SYSTEMD) $(_HAS_DINIT)),)
_SESSION_EXEC := bakawm-session
_INSTALL_SESSION_SCRIPT := yes
else
_SESSION_EXEC := bakawm --tty-udev
_INSTALL_SESSION_SCRIPT := no
endif
else ifeq ($(SESSION_MODE),session)
_SESSION_EXEC := bakawm-session
_INSTALL_SESSION_SCRIPT := yes
else ifeq ($(SESSION_MODE),direct)
_SESSION_EXEC := bakawm --tty-udev
_INSTALL_SESSION_SCRIPT := no
else
$(error Unknown SESSION_MODE "$(SESSION_MODE)". Use "auto", "session", or "direct")
endif

.PHONY: all build install install-bin install-resources install-systemd install-dinit uninstall clean

all: build

build:
	$(CARGO) build $(CARGO_FLAGS) $(FEATURES)

install: install-bin install-resources

install-bin: build
	install -Dm755 target/release/bakawm $(DESTDIR)$(BINDIR)/bakawm
	install -Dm755 target/release/bakawm-ctl $(DESTDIR)$(BINDIR)/bakawm-ctl

install-resources: install-session install-portals

install-session:
ifeq ($(_INSTALL_SESSION_SCRIPT),yes)
	install -Dm755 resources/bakawm-session $(DESTDIR)$(BINDIR)/bakawm-session
endif
	@mkdir -p $(DESTDIR)$(WAYLAND_SESSIONS_DIR)
	sed 's|^Exec=.*|Exec=$(_SESSION_EXEC)|' resources/bakawm.desktop > $(DESTDIR)$(WAYLAND_SESSIONS_DIR)/bakawm.desktop
	chmod 644 $(DESTDIR)$(WAYLAND_SESSIONS_DIR)/bakawm.desktop

install-portals:
	install -Dm644 resources/bakawm-portals.conf $(DESTDIR)$(PORTAL_DIR)/bakawm-portals.conf
	install -Dm644 resources/bakawm.portal $(DESTDIR)$(PORTAL_DIR)/portals/bakawm.portal

install-systemd:
	install -Dm644 resources/bakawm.service $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/bakawm.service
	install -Dm644 resources/bakawm-shutdown.target $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/bakawm-shutdown.target

install-dinit:
	install -Dm644 resources/dinit/bakawm $(DESTDIR)$(DINIT_USER_DIR)/bakawm
	install -Dm644 resources/dinit/bakawm.target $(DESTDIR)$(DINIT_USER_DIR)/bakawm.target

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/bakawm
	rm -f $(DESTDIR)$(BINDIR)/bakawm-ctl
	rm -f $(DESTDIR)$(BINDIR)/bakawm-session
	rm -f $(DESTDIR)$(WAYLAND_SESSIONS_DIR)/bakawm.desktop
	rm -f $(DESTDIR)$(PORTAL_DIR)/bakawm-portals.conf
	rm -f $(DESTDIR)$(PORTAL_DIR)/portals/bakawm.portal
	rm -f $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/bakawm.service
	rm -f $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/bakawm-shutdown.target
	rm -f $(DESTDIR)$(DINIT_USER_DIR)/bakawm
	rm -f $(DESTDIR)$(DINIT_USER_DIR)/bakawm.target

clean:
	$(CARGO) clean