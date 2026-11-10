# Connecting Google Calendar (and optionally Gmail) to Almond

Almond is a personal, local-first app, so it does not ship a shared Google "app". You create
your **own** OAuth client in your own Google Cloud project (free, about 10 minutes). Your tokens
stay on your devices (OS keychain / Android Keystore) and are never synced.

What Almond asks for:

| Scope | Why | Google's class |
|---|---|---|
| `openid`, `email` | identify which account you connected | non-sensitive |
| `calendar.readonly` | read events to show upcoming meetings and remind you | sensitive |
| `gmail.readonly` (optional, per account) | show recent threads with meeting attendees | restricted |

Almond never writes to your calendar or mail in this version.

> Vietnamese version below / Bản tiếng Việt ở phía dưới.

---

## English

### 1. Create a Google Cloud project
1. Open <https://console.cloud.google.com/> and sign in with any Google account (it does not have
   to be the one whose calendar you will connect).
2. Top bar → project picker → **New project**. Name it e.g. `almond-personal` → **Create**, then
   make sure it is selected.

### 2. Enable the APIs
1. **APIs & Services → Library**.
2. Search **Google Calendar API** → **Enable**.
3. (Optional, only if you want mail context) search **Gmail API** → **Enable**.

### 3. Configure the OAuth consent screen
In the current console this lives under **Google Auth Platform** (older consoles: **APIs & Services
→ OAuth consent screen**).
1. **Branding / App information**: app name `Almond`, your email as support and developer contact.
2. **Audience / User type**: choose **External** (a personal @gmail.com account cannot pick
   Internal). Save.
3. **Data access / Scopes** → *Add or remove scopes* and add:
   `openid`, `.../auth/userinfo.email`, `.../auth/calendar.readonly`, and optionally
   `.../auth/gmail.readonly`. Save.
4. **Audience → Test users** → **Add users** → add every Google account you plan to connect
   (needed while the app is in *Testing*).

### 4. Publish the app to "In production" (important)
While the app is in **Testing**, Google expires refresh tokens after **7 days** and Almond would
ask you to sign in again every week.
1. **Audience → Publishing status → Publish app** → confirm. Status becomes **In production**.
2. You do **not** need to submit for verification for personal use (the limit is 100 users).
   Leave the "Prepare for verification" prompts alone.
3. When you sign in later, Google shows **"Google hasn't verified this app"**. This is expected for
   your own unverified app: click **Advanced → Go to Almond (unsafe)**, then **Continue**. Only do
   this for the client you created yourself.

### 5. Create the Desktop OAuth client
1. **Clients** (older: **Credentials → Create credentials → OAuth client ID**) → **Create client**.
2. **Application type: Desktop app**. Name: `Almond desktop` → **Create**.
3. Copy the **Client ID** (ends in `.apps.googleusercontent.com`) and the **Client secret**.
   For a Desktop client the secret is not truly secret, but still don't post it publicly.
   Do not commit the downloaded `client_secret_*.json` anywhere (the repo `.gitignore` blocks it).

No redirect URI needs to be configured: Almond uses the loopback flow
(`http://127.0.0.1:<random port>`) with PKCE, which Desktop clients allow automatically.

### 6. Enter the client in Almond and connect accounts
1. In Almond open **Settings → Calendar & mail → Google** and paste the **Client ID** and
   **Client secret**. (Same values on every device: PC, laptop, phone.)
2. Click **Add Google account**. Your browser opens Google's sign-in; pick the account, click
   through the unverified-app warning (step 4.3), and allow the requested access.
   Tick the Gmail box first if you want mail context for that account.
3. The browser shows "Almond is connected to Google"; close the tab. Repeat for each account.
4. To disconnect, use **Remove** next to the account: Almond revokes the token at Google and
   deletes it locally. You can also revoke at <https://myaccount.google.com/permissions>.

### Troubleshooting
- **"Sign in again" after a week** → the app is still in *Testing*; do step 4, then reconnect.
- **`access_denied` / "app is blocked"** → the account is not a test user (step 3.4) or the
  scope wasn't added (step 3.3).
- **`invalid_client`** → client id/secret mistyped, or you created a *Web* client instead of a
  *Desktop* client.
- **No Gmail threads** → the account was connected without the Gmail box, or you unticked Gmail on
  the consent screen; remove and re-add the account with Gmail ticked.
- **Developer test**: `ALMOND_GOOGLE_CLIENT_ID=… ALMOND_GOOGLE_CLIENT_SECRET=… cargo test -p
  almond-calendar --test oauth -- --ignored --nocapture` prints a URL to open and runs a real
  sign-in (then removes the account again).

---

## Tiếng Việt

Almond là ứng dụng cá nhân, chạy cục bộ, nên không có "ứng dụng Google" dùng chung. Bạn tự tạo
**OAuth client riêng** trong dự án Google Cloud của mình (miễn phí, khoảng 10 phút). Token chỉ
nằm trên thiết bị của bạn (keychain của hệ điều hành / Android Keystore) và không bao giờ được
đồng bộ.

Quyền Almond yêu cầu: `openid`, `email` (biết bạn đã kết nối tài khoản nào), `calendar.readonly`
(đọc lịch để hiển thị cuộc họp sắp tới và nhắc nhở), `gmail.readonly` (tuỳ chọn, theo từng tài
khoản: hiển thị các email gần đây với người tham dự). Phiên bản này không ghi gì vào lịch hay email.

### 1. Tạo dự án Google Cloud
1. Mở <https://console.cloud.google.com/> và đăng nhập (tài khoản nào cũng được).
2. Thanh trên cùng → chọn dự án → **New project** (Dự án mới). Đặt tên, ví dụ `almond-personal`
   → **Create**, rồi chọn dự án đó.

### 2. Bật API
1. **APIs & Services → Library** (Thư viện).
2. Tìm **Google Calendar API** → **Enable** (Bật).
3. (Tuỳ chọn, nếu muốn ngữ cảnh email) tìm **Gmail API** → **Enable**.

### 3. Cấu hình màn hình đồng ý OAuth (OAuth consent screen)
Trong giao diện mới, mục này nằm ở **Google Auth Platform** (giao diện cũ: **APIs & Services →
OAuth consent screen**).
1. **Branding**: tên ứng dụng `Almond`, email của bạn làm email hỗ trợ và liên hệ nhà phát triển.
2. **Audience / User type**: chọn **External** (tài khoản @gmail.com cá nhân không chọn được
   Internal). Lưu.
3. **Data access / Scopes** → *Add or remove scopes*, thêm: `openid`,
   `.../auth/userinfo.email`, `.../auth/calendar.readonly`, và tuỳ chọn
   `.../auth/gmail.readonly`. Lưu.
4. **Audience → Test users** → **Add users** → thêm mọi tài khoản Google bạn định kết nối.

### 4. Chuyển ứng dụng sang "In production" (quan trọng)
Khi ứng dụng ở trạng thái **Testing**, Google cho refresh token hết hạn sau **7 ngày**, nên tuần
nào Almond cũng bắt bạn đăng nhập lại.
1. **Audience → Publishing status → Publish app** → xác nhận. Trạng thái thành **In production**.
2. **Không** cần gửi xác minh (verification) khi dùng cá nhân (giới hạn 100 người dùng). Bỏ qua
   các lời nhắc "Prepare for verification".
3. Khi đăng nhập, Google sẽ hiện **"Google hasn't verified this app"** (Google chưa xác minh
   ứng dụng này). Đây là bình thường với ứng dụng của chính bạn: bấm **Advanced (Nâng cao) → Go
   to Almond (unsafe)**, rồi **Continue**. Chỉ làm vậy với client do chính bạn tạo.

### 5. Tạo OAuth client loại Desktop
1. **Clients** (giao diện cũ: **Credentials → Create credentials → OAuth client ID**) →
   **Create client**.
2. **Application type: Desktop app**. Tên: `Almond desktop` → **Create**.
3. Sao chép **Client ID** (kết thúc bằng `.apps.googleusercontent.com`) và **Client secret**.
   Đừng đăng công khai, và đừng commit file `client_secret_*.json` vào git.

Không cần khai báo redirect URI: Almond dùng luồng loopback (`http://127.0.0.1:<cổng ngẫu
nhiên>`) với PKCE, client Desktop tự cho phép.

### 6. Nhập client vào Almond và kết nối tài khoản
1. Trong Almond mở **Settings → Calendar & mail → Google**, dán **Client ID** và **Client secret**
   (dùng cùng giá trị trên mọi thiết bị: PC, laptop, điện thoại).
2. Bấm **Add Google account**. Trình duyệt mở trang đăng nhập Google; chọn tài khoản, vượt qua
   cảnh báo ứng dụng chưa xác minh (bước 4.3) và cho phép truy cập. Đánh dấu ô Gmail trước nếu
   muốn ngữ cảnh email cho tài khoản đó.
3. Trình duyệt hiện "Almond is connected to Google"; đóng tab. Lặp lại cho từng tài khoản.
4. Để ngắt kết nối, bấm **Remove** cạnh tài khoản: Almond thu hồi token ở Google và xoá trên máy.
   Bạn cũng có thể thu hồi tại <https://myaccount.google.com/permissions>.

### Xử lý sự cố
- **Mỗi tuần phải đăng nhập lại** → ứng dụng vẫn ở *Testing*; làm bước 4 rồi kết nối lại.
- **`access_denied` / "app is blocked"** → tài khoản chưa là test user (bước 3.4) hoặc chưa thêm
  scope (bước 3.3).
- **`invalid_client`** → nhập sai client id/secret, hoặc bạn tạo client *Web* thay vì *Desktop*.
- **Không thấy email** → tài khoản được kết nối mà không chọn Gmail; xoá và thêm lại với ô Gmail.
