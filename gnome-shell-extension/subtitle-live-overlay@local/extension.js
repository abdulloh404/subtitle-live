'use strict';

const {Clutter, Gio, GLib, Pango, St} = imports.gi;
const Main = imports.ui.main;

const BUS_NAME = 'io.github.subtitle_live';
const OBJECT_PATH = '/io/github/subtitle_live/Overlay';
const FINAL_HOLD_MS = 4000;
const SCREEN_MARGIN_PX = 32;
const REGISTER_RETRY_MS = 200;
const MAX_REGISTER_ATTEMPTS = 25;

const INTERFACE_XML = `
<node>
  <interface name="io.github.subtitle_live.Overlay1">
    <method name="RegisterRenderer">
      <arg name="enabled" type="b" direction="out"/>
      <arg name="visible" type="b" direction="out"/>
      <arg name="text" type="s" direction="out"/>
      <arg name="is_final" type="b" direction="out"/>
      <arg name="position" type="s" direction="out"/>
      <arg name="font_size" type="u" direction="out"/>
      <arg name="width_px" type="u" direction="out"/>
      <arg name="background_opacity" type="d" direction="out"/>
      <arg name="max_lines" type="u" direction="out"/>
    </method>
    <method name="UnregisterRenderer"/>
    <signal name="Show">
      <arg name="text" type="s"/>
      <arg name="is_final" type="b"/>
    </signal>
    <signal name="Hide"/>
    <signal name="Configure">
      <arg name="enabled" type="b"/>
      <arg name="position" type="s"/>
      <arg name="font_size" type="u"/>
      <arg name="width_px" type="u"/>
      <arg name="background_opacity" type="d"/>
      <arg name="max_lines" type="u"/>
    </signal>
  </interface>
</node>`;

const OverlayProxy = Gio.DBusProxy.makeProxyWrapper(INTERFACE_XML);

/// actor ของ Shell ไม่ใช่หน้าต่าง จึงไม่เข้า Overview, Alt-Tab หรือ taskbar
class SubtitleLiveOverlayExtension {
    constructor() {
        this._enabled = false;
        this._requestedVisible = false;
        this._registered = false;
        this._registering = false;
        this._inOverview = false;
        this._position = 'bottom-center';
        this._fontSize = 28;
        this._widthPx = 960;
        this._backgroundOpacity = 0.6;
        this._maxLines = 2;
        this._finalTimerId = 0;
        this._registerRetryId = 0;
        this._registerAttempts = 0;
        this._signalIds = [];
        this._ownerChangedId = 0;
        this._overviewShowingId = 0;
        this._overviewHiddenId = 0;
        this._monitorsChangedId = 0;
        this._proxy = null;
        this._cancellable = null;
        this._root = null;
        this._surface = null;
        this._label = null;
    }

    enable() {
        this._createActor();
        this._inOverview = Main.overview.visible;
        this._overviewShowingId = Main.overview.connect('showing', () => {
            this._inOverview = true;
            this._syncVisibility();
        });
        this._overviewHiddenId = Main.overview.connect('hidden', () => {
            this._inOverview = false;
            this._syncVisibility();
        });
        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed',
            () => this._updateMonitorGeometry()
        );

        this._cancellable = new Gio.Cancellable();
        this._proxy = new OverlayProxy(
            Gio.DBus.session,
            BUS_NAME,
            OBJECT_PATH,
            (proxy, error) => this._onProxyReady(proxy, error),
            this._cancellable
        );
    }

    disable() {
        if (this._registered && this._proxy && this._proxy.g_name_owner)
            this._proxy.UnregisterRendererRemote();

        this._cancelFinalTimer();
        this._cancelRegisterRetry();
        if (this._cancellable)
            this._cancellable.cancel();
        if (this._proxy) {
            for (const signalId of this._signalIds)
                this._proxy.disconnectSignal(signalId);
            if (this._ownerChangedId)
                this._proxy.disconnect(this._ownerChangedId);
        }
        if (this._overviewShowingId)
            Main.overview.disconnect(this._overviewShowingId);
        if (this._overviewHiddenId)
            Main.overview.disconnect(this._overviewHiddenId);
        if (this._monitorsChangedId)
            Main.layoutManager.disconnect(this._monitorsChangedId);

        if (this._root)
            this._root.destroy();
        this._root = null;
        this._surface = null;
        this._label = null;
        this._proxy = null;
        this._cancellable = null;
        this._signalIds = [];
        this._registered = false;
        this._registering = false;
    }

    /// สร้าง root คงที่หนึ่งครั้ง แล้วให้ BinLayout จัดขนาดข้อความโดยไม่วัด layout แบบ synchronous
    _createActor() {
        this._label = new St.Label({
            text: '',
            reactive: false,
            can_focus: false,
        });
        this._label.clutter_text.set_line_alignment(Pango.Alignment.CENTER);
        this._label.clutter_text.set_line_wrap(false);
        this._label.clutter_text.set_ellipsize(Pango.EllipsizeMode.NONE);

        this._surface = new St.BoxLayout({
            reactive: false,
            can_focus: false,
            x_expand: true,
            y_expand: true,
            margin_top: SCREEN_MARGIN_PX,
            margin_right: SCREEN_MARGIN_PX,
            margin_bottom: SCREEN_MARGIN_PX,
            margin_left: SCREEN_MARGIN_PX,
        });
        this._surface.add_child(this._label);

        this._root = new St.Widget({
            reactive: false,
            can_focus: false,
            visible: false,
            layout_manager: new Clutter.BinLayout(),
        });
        this._root.add_child(this._surface);
        // เพิ่มตรงบน uiGroup เพื่อให้อยู่เหนือหน้าต่างโดยไม่ให้ LayoutManager
        // คำนวณ input region/strut ใหม่ทุกครั้งที่ข้อความเปลี่ยน
        Main.uiGroup.add_child(this._root);
        this._applyStyle();
        this._applyPosition();
        this._updateMonitorGeometry();
    }

    /// เริ่มฟัง signal หลัง proxy พร้อม แม้แอปอาจยังไม่ได้เปิด
    _onProxyReady(proxy, error) {
        if (!this._surface || error) {
            if (error && !error.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                logError(error, 'Subtitle-live: เชื่อมต่อ D-Bus overlay ไม่ได้');
            return;
        }
        this._signalIds.push(proxy.connectSignal('Show', (_proxy, _sender, args) => {
            this._show(args[0], args[1]);
        }));
        this._signalIds.push(proxy.connectSignal('Hide', () => this._hide()));
        this._signalIds.push(proxy.connectSignal('Configure', (_proxy, _sender, args) => {
            this._configure(...args);
        }));
        this._ownerChangedId = proxy.connect(
            'notify::g-name-owner',
            () => this._onNameOwnerChanged()
        );
        this._onNameOwnerChanged();
    }

    /// register ใหม่ทุกครั้งที่แอปเริ่มหรือเปลี่ยน D-Bus owner
    _onNameOwnerChanged() {
        if (!this._proxy || !this._proxy.g_name_owner) {
            this._cancelRegisterRetry();
            this._registerAttempts = 0;
            this._registered = false;
            this._registering = false;
            this._requestedVisible = false;
            this._syncVisibility();
            return;
        }
        if (this._registered || this._registering)
            return;

        this._registerRenderer();
    }

    /// ลอง register ซ้ำเมื่อ GApplication ได้ชื่อ bus แล้วแต่ยังสร้าง Overlay object ไม่เสร็จ
    _registerRenderer() {
        if (!this._proxy || !this._proxy.g_name_owner || this._registering)
            return;
        this._registering = true;
        this._registerAttempts++;
        this._proxy.RegisterRendererRemote(
            this._cancellable,
            (result, error) => {
                this._registering = false;
                if (!this._surface || error || !this._proxy.g_name_owner) {
                    if (error &&
                        this._proxy &&
                        this._proxy.g_name_owner &&
                        error.matches(Gio.DBusError, Gio.DBusError.UNKNOWN_METHOD) &&
                        this._registerAttempts < MAX_REGISTER_ATTEMPTS) {
                        this._scheduleRegisterRetry();
                    } else if (error &&
                               !error.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED)) {
                        logError(error, 'Subtitle-live: ใช้ GTK fallback เพราะ register Shell renderer ไม่ได้');
                    }
                    return;
                }
                this._cancelRegisterRetry();
                this._registerAttempts = 0;
                this._registered = true;
                const [
                    enabled,
                    visible,
                    text,
                    isFinal,
                    position,
                    fontSize,
                    widthPx,
                    backgroundOpacity,
                    maxLines,
                ] = result;
                this._configure(
                    enabled,
                    position,
                    fontSize,
                    widthPx,
                    backgroundOpacity,
                    maxLines
                );
                if (visible && text)
                    this._show(text, isFinal);
                else
                    this._hide();
            }
        );
    }

    _scheduleRegisterRetry() {
        if (this._registerRetryId || !this._proxy || !this._proxy.g_name_owner)
            return;
        this._registerRetryId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            REGISTER_RETRY_MS,
            () => {
                this._registerRetryId = 0;
                this._registerRenderer();
                return GLib.SOURCE_REMOVE;
            }
        );
    }

    _cancelRegisterRetry() {
        if (this._registerRetryId) {
            GLib.source_remove(this._registerRetryId);
            this._registerRetryId = 0;
        }
    }

    /// นำ config จากแอปมาใช้โดยไม่เก็บค่าซ้ำใน Extension
    _configure(enabled, position, fontSize, widthPx, backgroundOpacity, maxLines) {
        this._enabled = enabled;
        this._position = position;
        this._fontSize = fontSize;
        this._widthPx = widthPx;
        this._backgroundOpacity = Math.max(0, Math.min(1, backgroundOpacity));
        this._maxLines = maxLines;
        this._applyStyle();
        this._applyPosition();
        this._syncVisibility();
    }

    /// แสดง plain text ที่ Rust จัดบรรทัดและดันบรรทัดมาแล้ว
    _show(text, isFinal) {
        this._cancelFinalTimer();
        // final frame อาจมีข้อความเดิม จึงไม่สั่ง relayout ซ้ำโดยไม่จำเป็น
        if (this._label.get_text() !== text)
            this._label.set_text(text);
        this._requestedVisible = Boolean(text);
        this._syncVisibility();
        if (isFinal && this._requestedVisible) {
            this._finalTimerId = GLib.timeout_add(
                GLib.PRIORITY_DEFAULT,
                FINAL_HOLD_MS,
                () => {
                    this._finalTimerId = 0;
                    this._requestedVisible = false;
                    this._syncVisibility();
                    return GLib.SOURCE_REMOVE;
                }
            );
        }
    }

    /// ซ่อนและล้างข้อความทันทีเมื่อ pipeline หยุดหรือเงียบนานเกินกำหนด
    _hide() {
        this._cancelFinalTimer();
        this._requestedVisible = false;
        if (this._label && this._label.get_text())
            this._label.set_text('');
        this._syncVisibility();
    }

    _cancelFinalTimer() {
        if (this._finalTimerId) {
            GLib.source_remove(this._finalTimerId);
            this._finalTimerId = 0;
        }
    }

    /// ใช้ inline style เพื่อให้ค่าจากหน้า Settings เปลี่ยนได้ทันที
    _applyStyle() {
        if (!this._surface || !this._label)
            return;
        this._surface.set_style(
            `background-color: rgba(0, 0, 0, ${this._backgroundOpacity.toFixed(3)}); ` +
            `border-radius: 12px; padding: 12px 20px; max-width: ${this._widthPx}px;`
        );
        this._label.set_style(
            `color: white; font-size: ${this._fontSize}pt; font-weight: 600;`
        );
    }

    /// root ครอบเฉพาะจอหลักและเปลี่ยน geometry เฉพาะเมื่อชุดจอเปลี่ยน
    _updateMonitorGeometry() {
        if (!this._root)
            return;
        const monitor = Main.layoutManager.primaryMonitor || Main.layoutManager.monitors[0];
        if (!monitor)
            return;
        this._root.set_position(monitor.x, monitor.y);
        this._root.set_size(monitor.width, monitor.height);
    }

    /// ให้ BinLayout จัดตำแหน่งตาม anchor โดยพื้นหลังยังขยายตามข้อความจนถึง max-width
    _applyPosition() {
        if (!this._surface)
            return;
        let horizontal = Clutter.ActorAlign.CENTER;
        if (this._position.endsWith('-left'))
            horizontal = Clutter.ActorAlign.START;
        else if (this._position.endsWith('-right'))
            horizontal = Clutter.ActorAlign.END;

        let vertical = Clutter.ActorAlign.CENTER;
        if (this._position.startsWith('top-'))
            vertical = Clutter.ActorAlign.START;
        else if (this._position.startsWith('bottom-'))
            vertical = Clutter.ActorAlign.END;
        this._surface.set_x_align(horizontal);
        this._surface.set_y_align(vertical);
    }

    /// ซ่อนใน Overview และแสดงเฉพาะเมื่อแอปกำลังส่ง subtitle จริง
    _syncVisibility() {
        if (!this._root)
            return;
        const shouldShow = this._registered &&
            this._enabled &&
            this._requestedVisible &&
            !this._inOverview;
        if (shouldShow)
            this._root.show();
        else
            this._root.hide();
    }
}

function init() {
    return new SubtitleLiveOverlayExtension();
}
