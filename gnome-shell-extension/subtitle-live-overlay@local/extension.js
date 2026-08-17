"use strict";

const { Meta } = imports.gi;

const OVERLAY_TITLE = "Subtitle-live Overlay";

/// Extension นี้ไม่วาดหรือจัดวาง subtitle เอง
/// หน้าที่เดียวคือให้ Mutter วางหน้าต่าง GTK overlay ไว้เหนือหน้าต่างอื่น
class SubtitleLiveAlwaysOnTopExtension {
  constructor() {
    this._windowMappedId = 0;
    this._restackedId = 0;
    this._windows = new Map();
  }

  enable() {
    log("Subtitle-live: เปิด always-on-top Extension แล้ว");
    // รอให้ Mutter นำหน้าต่างเข้า stack ก่อนจึงค่อยสั่ง make_above
    this._windowMappedId = global.window_manager.connect_after(
      "map",
      (_windowManager, actor) => this._trackWindow(actor.get_meta_window()),
    );
    // Mutter ส่ง restacked หลังจัด scene graph แต่ก่อนวาดเฟรมใหม่
    // จึงวาง actor บนสุดตรงนี้ทันที โดยไม่เรียก window.raise() ให้เกิด event วน
    this._restackedId = global.display.connect("restacked", () =>
      this._keepOverlayActorsOnTop(),
    );

    // รองรับกรณีเปิดโปรแกรมอยู่ก่อนเปิด Extension
    for (const actor of global.get_window_actors())
      this._trackWindow(actor.get_meta_window());
  }

  disable() {
    if (this._windowMappedId)
      global.window_manager.disconnect(this._windowMappedId);
    this._windowMappedId = 0;
    if (this._restackedId) global.display.disconnect(this._restackedId);
    this._restackedId = 0;

    for (const [window, state] of this._windows) {
      if (state.laterId) Meta.later_remove(state.laterId);
      if (state.titleChangedId) window.disconnect(state.titleChangedId);
      if (state.aboveChangedId) window.disconnect(state.aboveChangedId);
      if (state.unmanagedId) window.disconnect(state.unmanagedId);
      if (state.pinned && !state.wasAbove) window.unmake_above();
      if (state.pinned && !state.wasSticky) window.unstick();
    }
    this._windows.clear();
  }

  /// ติดตาม title ไว้ด้วย เผื่อ backend ส่ง map มาก่อน GTK กำหนด title เสร็จ
  _trackWindow(window) {
    if (!window || this._windows.has(window)) return;

    const state = {
      pinned: false,
      laterId: 0,
      titleChangedId: 0,
      aboveChangedId: 0,
      unmanagedId: 0,
      wasAbove: false,
      wasSticky: false,
    };
    state.titleChangedId = window.connect("notify::title", () =>
      this._pinIfOverlay(window),
    );
    state.unmanagedId = window.connect("unmanaged", () => {
      if (state.laterId) Meta.later_remove(state.laterId);
      this._windows.delete(window);
    });
    this._windows.set(window, state);
    this._pinIfOverlay(window);
  }

  /// ให้ GNOME Shell คุมเฉพาะ stacking; ขนาดและตำแหน่งยังมาจาก GTK ทั้งหมด
  _pinIfOverlay(window) {
    const state = this._windows.get(window);
    if (
      !state ||
      state.pinned ||
      state.laterId ||
      !this._isOverlayWindow(window)
    )
      return;

    // map เสร็จแล้วแต่ stack อาจยัง sync ไม่ครบ จึงรอถึงก่อนวาดเฟรม
    state.laterId = Meta.later_add(Meta.LaterType.BEFORE_REDRAW, () => {
      state.laterId = 0;
      if (!this._windows.has(window) || !this._isOverlayWindow(window))
        return false;

      state.wasAbove = window.is_above();
      state.wasSticky = window.is_on_all_workspaces();
      window.stick();
      window.make_above();
      state.pinned = true;
      this._placeActorOnTop(window);

      state.aboveChangedId = window.connect("notify::above", () => {
        if (state.pinned && !window.is_above()) {
          window.make_above();
          this._placeActorOnTop(window);
        }
      });

      log(
        `Subtitle-live: ส่งคำสั่ง always-on-top ให้ "${window.get_title()}" แล้ว ` +
          `(above=${window.is_above()}, layer=${window.get_layer()}, ` +
          `application_id=${window.get_gtk_application_id() || "ไม่มี"})`,
      );

      // เมื่อพบหน้าต่างเป้าหมายแล้ว ไม่ต้องทำงานอีกทุกครั้งที่ title เปลี่ยน
      if (state.titleChangedId) {
        window.disconnect(state.titleChangedId);
        state.titleChangedId = 0;
      }
      return false;
    });
  }

  /// คงภาพ overlay ไว้บนสุดหลัง Mutter จัด stack โดยไม่แก้ Meta stack ซ้ำ
  _keepOverlayActorsOnTop() {
    for (const [window, state] of this._windows) {
      if (!state.pinned || !this._isOverlayWindow(window)) continue;
      if (!window.is_above()) window.make_above();
      this._placeActorOnTop(window);
    }
  }

  /// ย้ายเฉพาะตัววาดของหน้าต่างขึ้นบนสุดใน parent เดิม จึงไม่ส่ง restacked ใหม่
  _placeActorOnTop(window) {
    const actor = window.get_compositor_private();
    const parent = actor ? actor.get_parent() : null;
    if (parent) parent.set_child_above_sibling(actor, null);
  }

  _isOverlayWindow(window) {
    // title นี้ถูกกำหนดเฉพาะให้ GTK overlay; ไม่พึ่ง application id
    // เพราะ Mutter แต่ละ backend อาจรายงานค่านี้ไม่เหมือนกัน
    return window.get_title() === OVERLAY_TITLE;
  }
}

function init() {
  return new SubtitleLiveAlwaysOnTopExtension();
}
