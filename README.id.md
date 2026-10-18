# VPN Splitter

[English](README.md) | [Bahasa Indonesia](README.id.md)

Arahkan trafik IPv4 kantor melalui profil VPN L2TP yang tersimpan, sementara
internet umum tetap lewat Wi-Fi atau Ethernet. Aplikasi desktop ini dibuat
dengan Rust dan GPUI, serta bisa diakses dari ikon di menu bar macOS. Kamu bisa
mengelola koneksi VPN dan aturan routing lewat Settings.

## Dukungan platform

| Platform | Status |
| --- | --- |
| macOS | Aplikasi desktop lokal dan helper routing dengan izin administrator; diuji di Apple Silicon. Bundle memerlukan macOS 14 atau lebih baru. |
| Windows | Binary bisa dibangun. Pengelolaan koneksi VPN dan helper routing belum diimplementasikan. |

Routing IPv6, URL lengkap, dan tujuan wildcard seperti `*.office.example` belum
didukung. Tambahkan setiap hostname yang dibutuhkan; aplikasi tidak mencari
subdomain secara otomatis. Lihat [catatan validasi](docs/desktop-validation.md)
untuk hasil pengukuran dan pengujian yang masih perlu dilakukan.

## Build di macOS

Pasang Xcode beserta Command Line Tools, Rust, dan GNU Make. Aplikasi desktop
pernah dibangun menggunakan Rust 1.97.1. Jalankan dari direktori utama repo:

```sh
git clone https://github.com/Candratama/vpn-splitter.git
cd vpn-splitter
make macos
open "target/release/VPN Splitter.app"
```

`make macos` membangun dan menandatangani aplikasi lokal. Build atau membuka
aplikasi belum memasang helper dengan hak administrator. Build lokal bawaan
menggunakan tanda tangan ad hoc dan belum dinotarisasi oleh Apple; kamu tidak
memerlukan akun Apple Developer untuk build ini.

Untuk tanda tangan developer, atur `VPN_SPLITTER_SIGN_IDENTITY` di environment
lokal sebelum build. Proses packaging tidak mengunggah atau menotarisasi aplikasi.

## Instalasi dan koneksi

1. Tambahkan profil VPN L2TP di System Settings macOS dan matikan **Send all traffic**.
2. Buka Settings dari ikon aplikasi di menu bar, lalu masuk ke bagian **Helper**.
3. Pilih **Install / approve helper...** dan setujui dialog administrator macOS.
   Masukkan password hanya di dialog tersebut.
4. Build lokal dipasang di
   `/Library/PrivilegedHelperTools/VPN Splitter.app`. Aplikasi preview akan
   tertutup dan aplikasi yang terpasang terbuka secara otomatis.
5. Di bagian **VPN**, pilih profil yang tersimpan. Di **Routing rules**, masukkan
   atau impor tujuan kantor, lalu pilih **Save changes**.
6. Pilih **Connect VPN**. Pastikan routing sudah aktif sebelum mengakses server
   kantor. Gunakan **Disconnect VPN** setelah selesai.

Daftar tujuan menerima satu alamat IPv4, CIDR, atau hostname per baris:

```text
198.51.100.7
198.51.100.128/25
git.office.example
```

Alamat di atas hanya contoh dokumentasi. Ganti dengan tujuan milikmu.
Hostname harus berhasil diterjemahkan menjadi IPv4 sebelum rutenya diterapkan.
Status koneksi VPN dan routing ditampilkan terpisah. Quit membersihkan rute
milik aplikasi dan memutus koneksi VPN yang dimulai oleh aplikasi.

## Memperbarui atau menghapus helper

Untuk memperbarui, putuskan VPN dan tutup aplikasi yang terpasang. Build preview
baru dengan `make macos`, buka, lalu pilih **Install / approve helper...**.
Pembaruan memerlukan persetujuan administrator dan berhenti jika pembersihan
rute milik aplikasi gagal.

Untuk menghapus helper, putuskan koneksi terlebih dahulu, lalu pilih
**Remove helper...** di bagian **Helper**. Proses ini menghentikan layanan dan
memeriksa pembersihan rute. Jika gagal, file layanan dan jurnal pemulihan tetap
disimpan agar bisa dicoba ulang. Aplikasi yang terpasang, profil VPN, kredensial,
dan konfigurasi pengguna tetap disimpan.

## Perintah build

Jalankan `make` atau `make help` untuk melihat daftar perintah.

| Perintah | Hasil |
| --- | --- |
| `make build` | Binary release untuk toolchain Rust aktif di `target/release/`. |
| `make macos` | Bundle lokal bertanda tangan di `target/release/VPN Splitter.app`. Memerlukan macOS. |
| `make windows` | Binary Windows MinGW di `target/release/windows/`. Memerlukan Bash. |
| `make test` | Test regresi Rust dan shell. Memerlukan Bash. |
| `make clean` | Menghapus hasil build Cargo. |

Untuk cross-compilation Windows, pasang toolchain MinGW-w64 yang menyediakan
`x86_64-w64-mingw32-gcc`, lalu jalankan:

```sh
rustup target add x86_64-pc-windows-gnu
make windows
```

Di Windows dengan GNU Make dan tool build Rust native yang sudah terpasang,
`make build` memakai toolchain aktif dan menghasilkan file `.exe` di
`target/release/`. Build Windows saat ini belum bisa mengelola VPN.

Tanpa Make, jalankan `bash scripts/package-macos.sh` atau
`bash scripts/build-windows.sh` secara langsung. Untuk build binary native,
jalankan `cargo build --release --locked -p vpn-splitter-desktop -p vpn-splitter-helper`.

## CLI untuk macOS

Script shell bisa dipakai tanpa build aplikasi desktop. Hubungkan VPN terlebih
dahulu, lalu siapkan daftar tujuan lokal:

```sh
cp config.example.txt config.txt
# Edit config.txt dengan tujuan kantor sebelum melanjutkan.
./vpn-splitter.sh status
DRY_RUN=1 ./vpn-splitter.sh apply
sudo -v
./vpn-splitter.sh apply
./vpn-splitter.sh clear
```

CLI mempertahankan default route VPN dan menambahkan dua rute internet yang
lebih spesifik lewat Wi-Fi atau Ethernet. Rute kantor memakai VPN. Jalur DNS
IPv4 yang ada tetap dipertahankan; pengaturan DNS sistem dan routing IPv6 tidak
diubah.

Script hanya mencatat rute yang dibuatnya. Apply yang gagal memicu rollback;
pembersihan yang gagal menyimpan state agar bisa dicoba ulang. Apply berulang
tidak mengubah apa pun selama masih aktif. Setelah mengedit tujuan, menyambung
ulang VPN, atau berganti jaringan, jalankan `clear` lalu `apply`. `DRY_RUN=1`
bisa dipakai pada kedua perintah tanpa mengubah rute.

## Data lokal dan diagnostik

Konfigurasi desktop disimpan di
`~/Library/Application Support/VPN Splitter/config.json`. Aplikasi tidak mengimpor
`config.txt` milik CLI secara otomatis. Gunakan **Routing rules** untuk mengimpor
daftar tujuan.

Simpan IP kantor, hostname, password, dan inventaris di komputer lokal. Repo
mengabaikan konfigurasi CLI, laporan audit, kredensial, log, dan state lokal.
Periksa file sebelum commit; jangan gunakan force-add untuk konfigurasi privat.
Diagnostik menyamarkan data secara default. Periksa hasil ekspor sebelum dibagikan.

## Pengembangan

`make test` menjalankan test Rust dan shell. Test shell memakai perintah OS tiruan
dan tidak mengubah rute aktif. Detail build dan pengujian yang belum selesai
tersedia di dokumen berikut:

- [Arsitektur](docs/desktop-architecture.md)
- [Validasi dan batasan](docs/desktop-validation.md)
- [Rencana implementasi](docs/superpowers/plans/2026-10-15-gpui-desktop.md)
- [Atribusi ikon](crates/desktop/assets/HUGEICONS-LICENSE.md)
