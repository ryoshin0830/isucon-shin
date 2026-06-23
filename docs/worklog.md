# 作業ログ — private-isu (Team 30)

計測 → 1改善 → ベンチ → コミット のサイクルで記録する。

## 2026-06-23

### 環境構築・アクセス
- SG `sg-0143afb4d92447bee` に自IP `210.172.130.69` の SSH(22)/HTTP(80) を許可（CLI: 一時認証 `Get AWS CLI credentials`）。
  - ⚠️ 環境再プロビジョンで IP/SG が当初連絡から変化。現行: サーバ `54.95.55.129`、AWSアカウント `251937262269`。
  - CLI での SSH 鍵取得は不可（SSM非管理 / Parameter Store に鍵なし / EC2 Instance Connect 権限なし）→ ダッシュボード "Get EC2 SSH key" でDL。`ssh isu` で接続。
- サーバ: Ubuntu 24.04 / 2 vCPU / 3.8GB。既定 Ruby 稼働。データ 1000 users / 10000 posts / 100000 comments。

### 言語切替: Ruby → Rust（全面書き直し）
- private-isu に Rust 実装は付属しないため、Go参照実装を正典に **axum + sqlx + askama** で全14ルートを移植。
- passhash = `sha512(password + ":" + sha512(account_name))` を `sha2` で厳密再現（openssl shell-out 廃止）。
  openssl 計算値とDB保存値の一致をスモークテストで確認。
- セッションはサーバ側(memory)+cookie。CSRF / multipart投稿 / 画像配信 / `/@account` 移植。
- ハマり: sqlx は MySQL `TIMESTAMP` を `NaiveDateTime` に decode 不可 → `DateTime<Utc>` + 接続時 `time_zone='+00:00'` で解決。
- systemd `isu-rust`（env.sh, 127.0.0.1:8080）。`isu-ruby` 停止・無効化。

### ベンチ結果の推移
| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 11:55 | Ruby(既定) | **566** | ベースライン |
| 12:07 | Rust(索引なし) | **0** | `pass:true` だが GET / · POST /login · /register がタイムアウト。success 747 / fail 54 |
| 12:13 | Rust + インデックス | **34,416** | 順位 #43 → **#18**。約 60x |

### 効いた改善
1. **インデックス追加**（`db/indexes.sql`）が決定打。初期状態は PK と `users.account_name` UNIQUE 以外に索引が無く、
   `makePosts` の `comments` 参照（post_idで100,000行を毎回全走査 × 投稿ごと）と一覧の `ORDER BY created_at` が全表走査でタイムアウトしていた。
   - `comments(post_id, created_at)`, `comments(user_id)`, `posts(created_at)`, `posts(user_id, created_at)`
   - Rust はtokioの高並列でこのボトルネックを Ruby より強く顕在化させ、索引なしだと 0 点だった。

### 次の改善候補（計測してから）
- GET / の N+1 解消（投稿一覧のコメント/ユーザをまとめて取得）。現状は Go 同様の N+1 のまま。
- 画像を DB(`posts.imgdata`) からファイル化し nginx で静的配信（x5 配点）。
- 静的ファイル(css/js/img)を nginx 直配信。
- MySQL `innodb_buffer_pool_size` 等のチューニング。
- alp / pt-query-digest を入れて次のボトルネックを数値で特定。
