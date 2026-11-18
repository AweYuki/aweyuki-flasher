# Aweyuki Flasher

Bộ công cụ Flash ROM Android đa nền tảng viết hoàn toàn bằng **Rust**, dùng chung một lõi nhị phân duy nhất (`aweyuki-core`) cho:

- **Desktop Native GUI (`aweyuki-gui`)**: Giao diện **`egui` + `eframe` trên nền `wgpu`** cho **Windows / macOS / Linux**.
- **CLI & Termux (`aweyuki`)**: Công cụ dòng lệnh cho **Windows / macOS / Linux / Termux (Android-to-Android qua cáp OTG + `termux-usb`)**.
- **Web Demo (`web-demo/`)**: Biên dịch chính giao diện Rust + lõi nhị phân sang **WebAssembly (`wasm32-unknown-unknown`)** chạy ngay trên trình duyệt kèm **Device Simulator** và **1-click Sample ROMs**.

---

## Tính năng nổi bật

1. **Tự động phát hiện phân vùng (Auto-Detect)**:
   - Nhận diện gói `.zip` (`payload.bin` OTA ZIP, Fastboot ROM ZIP với `fastboot-info.txt` / `flash_all.sh` / `.bat`, Recovery Sideload ZIP), `payload.bin` lẻ, `super.img` lẻ (raw hoặc sparse), thư mục ROM, hoặc `.img` lẻ.
   - Đọc trực tiếp magic header nhị phân: Android Sparse (`0xED26FF3A`), CrAU OTA (`CrAU`), Dynamic Super (`0x616C4467` / `0x414C5030`), Boot/InitBoot (`ANDROID!`), VendorBoot (`VNDRBOOT`), AVB (`AVB0`).
   - Tự động phân chia giai đoạn flash **Bootloader** -> tự reboot **`fastbootd`** cho các phân vùng động (`system`, `vendor`, `product`, `system_ext`, `odm`...) và ánh xạ slot `_a`/`_b`.
   - Tự động lọc bỏ lệnh khoá bootloader (`fastboot oem lock` / `flashing lock`) trong script OEM để chống brick.
2. **Chia nhỏ phân vùng (Partition Splitting & Super Resize)**:
   - **Sparse Chunking (`SparseSplitter`)**: Tự động cắt Raw -> Multi-Sparse hoặc cắt trực tiếp Sparse -> Multi-Sparse (`resparse`) khớp `max-download-size` (tương thích 100% với AOSP `simg2img`).
   - **Quản lý phân vùng động `super` (`fastbootd`)**: Tự xoá phân vùng `-cow` thu hồi dung lượng, `create-logical-partition` / `resize-logical-partition`.
3. **Hỗ trợ đầy đủ `payload.bin` / `super.img` / `adb sideload`**:
   - **`payload.bin` (`CrAU` v2)**: Giải mã Protobuf `DeltaArchiveManifest`, hỗ trợ `REPLACE`, `REPLACE_XZ`, `REPLACE_BZ`, `ZSTD`, `ZERO`, kiểm tra SHA-256 từng operation và toàn bộ phân vùng, cho phép trích xuất lẻ 1 phân vùng (`init_boot.img`, `boot.img`...).
   - **`super.img`**: Hỗ trợ flash nguyên khối (tự cắt chunk) hoặc **`lpunpack`** giải nén từng logical partition để flash qua `fastbootd` (tương thích 100% AOSP `lpunpack`).
   - **`adb sideload`**: Kiểm tra metadata OTA (`pre-device`, `ota-type`) và stream trực tiếp vào recovery.
4. **Tiện ích nâng cao**:
   - **Tự động patch `vbmeta`** (`--disable-verity --disable-verification` + patch cờ bit `AVB0` tại offset `120..124`).
   - **Trích xuất lẻ phân vùng** từ `payload.bin` / `super.img` / `.zip`.
   - **Xuất script flash độc lập**: `.sh` (Linux/macOS), `.bat` (Windows), và `flash_termux_otg.sh` (Termux OTG).

---

## Cài đặt & Chạy thử

### 1. Chạy kiểm thử lõi & đối chiếu chuẩn AOSP (`simg2img`, `lpunpack`)
```bash
cargo test --workspace
```

### 2. Chạy ứng dụng Desktop Native (`wgpu`)
```bash
cargo run --release -p aweyuki-gui
```

### 3. Sử dụng CLI (`aweyuki`)
```bash
# Liệt kê thiết bị Fastboot / ADB / Termux USB
cargo run -p aweyuki-cli -- devices --simulate

# Tạo ROM mẫu (payload OTA ZIP, payload.bin, super.img, fastboot ZIP, sideload ZIP)
cargo run -p aweyuki-cli -- generate-sample --kind payload-zip -o /tmp/demo-ota.zip

# Tự động phát hiện phân vùng & xem kế hoạch flash
cargo run -p aweyuki-cli -- inspect /tmp/demo-ota.zip --max-size 256K

# Chạy giả lập flash + tự cắt sparse chunk 256KB + tự patch vbmeta
cargo run -p aweyuki-cli -- flash /tmp/demo-ota.zip --simulate low-buffer --max-size 256K

# Trích xuất lẻ init_boot & boot từ payload.bin hoặc OTA ZIP
cargo run -p aweyuki-cli -- payload /tmp/demo-ota.zip --extract init_boot,boot -o /tmp/out_imgs

# Chia nhỏ file .img theo giới hạn max-download-size
cargo run -p aweyuki-cli -- split /tmp/out_imgs/system.img --max-size 256K -o /tmp/chunks

# Xuất script Termux OTG / Linux / Windows
cargo run -p aweyuki-cli -- export-script /tmp/demo-ota.zip --format termux -o /tmp/flash_termux.sh
```

### 4. Cài đặt trên Termux (Android-to-Android OTG)
```bash
bash scripts/termux-install.sh
aweyuki flash /sdcard/Download/rom.zip --termux-usb --max-size 128M
```

### 5. Build & chạy Web Demo (WASM)
```bash
./web-demo/build-web.sh
python3 -m http.server 8787 --directory web-demo
# Mở trình duyệt tại http://127.0.0.1:8787
```
