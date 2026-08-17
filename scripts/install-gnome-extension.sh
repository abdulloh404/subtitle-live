#!/usr/bin/env sh
# ติดตั้ง GNOME Shell Extension ที่คุม always-on-top ให้ user ปัจจุบันโดยไม่ใช้ sudo
set -eu

extension_uuid='subtitle-live-overlay@local'
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(dirname -- "$script_dir")
source_dir="$project_dir/gnome-shell-extension/$extension_uuid"
data_dir="${XDG_DATA_HOME:-${HOME}/.local/share}"
target_dir="$data_dir/gnome-shell/extensions/$extension_uuid"

extension_changed=0
if ! cmp -s "$source_dir/metadata.json" "$target_dir/metadata.json" 2>/dev/null ||
   ! cmp -s "$source_dir/extension.js" "$target_dir/extension.js" 2>/dev/null; then
    extension_changed=1
fi

install -d "$target_dir"
install -m 0644 "$source_dir/metadata.json" "$target_dir/metadata.json"
install -m 0644 "$source_dir/extension.js" "$target_dir/extension.js"

extension_active=0
if gnome-extensions enable "$extension_uuid" 2>/dev/null; then
    extension_active=1
else
    # GNOME Wayland ไม่สแกน Extension ใหม่ระหว่าง session จึงบันทึกให้เปิดใน login ถัดไป
    gjs -c '
        const {Gio} = imports.gi;
        const uuid = ARGV[0];
        const settings = new Gio.Settings({schema_id: "org.gnome.shell"});
        const enabled = settings.get_strv("enabled-extensions");
        if (!enabled.includes(uuid)) {
            enabled.push(uuid);
            settings.set_strv("enabled-extensions", enabled);
        }
        const disabled = settings.get_strv("disabled-extensions");
        if (disabled.includes(uuid))
            settings.set_strv(
                "disabled-extensions",
                disabled.filter(item => item !== uuid)
            );
        Gio.Settings.sync();
    ' "$extension_uuid"
fi

if [ "$extension_changed" -eq 1 ] || [ "$extension_active" -eq 0 ]; then
    printf '%s\n' 'ติดตั้ง Extension และตั้งค่าให้เปิดอัตโนมัติแล้ว'
    printf '%s\n' 'กรุณาออกจากระบบแล้วเข้าใหม่หนึ่งครั้งเพื่อโหลดโค้ดรุ่นล่าสุด'
else
    printf '%s\n' 'Subtitle-live always-on-top Extension เปิดใช้งานอยู่แล้ว'
fi
