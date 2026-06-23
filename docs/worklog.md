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

## 2026-06-23 (続き) — インフラ系チューニング

### 変更（1ラウンドにまとめて投入。すべて定番・低リスク）
1. **画像を nginx 直配信**（x5 配点・最大の山）
   - `posts.imgdata`(計1.3GB/10113枚) を `public/image/<id>.<ext>` にファイル化。
   - Rust: `get_image` で初回アクセス時に書き出し（以降 nginx 直）、`POST /` 投稿時も書き出し。
   - nginx: `location /image/ { try_files $uri @app; }`（ファイルがあれば nginx、無ければアプリにフォールバック）。
   - 全画像を事前に curl で pre-warm（ファイル化済み、ディスク 90% 使用・残1.5GB）。
2. **静的ファイルを nginx 直配信**: `/css /js /img /favicon.ico` を `root` から直接（`expires 1d`）。
3. **nginx チューニング**: `worker_connections 16384`, upstream keepalive 128（`proxy_http_version 1.1` + `Connection ""`), gzip, open_file_cache, tcp_nodelay。
4. **MySQL チューニング**（`zz-isucon.cnf`）: `innodb_buffer_pool_size`（実行値2G）, `innodb_flush_log_at_trx_commit=2`, `O_DIRECT`, `innodb_log_file_size=256M`。
5. **alp 導入**（v1.0.21）+ nginx LTSV ログ。

### 結果
| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 12:37 | + 画像nginx直配信 / 静的直配信 / MySQL・nginxチューニング | **57,371** | #28 → #22。+66% |

- 実行中の `top`: **mysqld 90% CPU** / isu-rust 72%。**次のボトルネックは MySQL = make_posts の N+1**。

### make_posts の N+1 解消（バッチ取得）
- 投稿ごとの「COUNT + コメント取得 + コメント毎ユーザ取得 + 投稿者取得」(~120 queries/page) を
  **3本の IN クエリ**に集約: コメント数 `GROUP BY post_id`、コメントは `post_id IN (...)` で取得し Rust で post 毎にバケット（最新3件）、ユーザは必要 id を `IN (...)` で一括取得しマップ化。
- 一覧クエリを `JOIN users del_flg=0 + LIMIT 20` に（全1万件フェッチを停止）。`/@account` も `LIMIT 20`。

| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 12:42 | + N+1 解消（バッチ取得） | **123,131** | #22 → #17。約 2.1x |

- 実行中: **mysqld 163% CPU**（2コア飽和）/ isu-rust 18%。ボトルネックは完全に MySQL CPU。

## 2026-06-23 (続き2) — MySQL CPU ボトルネックの解消

### 計測（slow query log + mysqldumpslow）
- `GET /` の一覧クエリが **363s（全体の最大）**、`GET /posts` ページングが 96s。1 クエリ **0.19s**（20行返すだけ）。
- EXPLAIN で判明: `posts JOIN users WHERE del_flg=0 ORDER BY created_at DESC LIMIT 20` が
  **users を全スキャン → 各ユーザの全投稿を結合 → temp table + filesort で1万件をソート → LIMIT**。
  LIMIT が効かず毎リクエスト1万件処理していた。

### 変更
1. **imgdata を posts から分離**（`db/split_imgdata.sql`）。images テーブルへ（既存分はファイル化済みなので空作成）、
   `ALTER TABLE posts DROP COLUMN imgdata` + `ENGINE=InnoDB` で物理リビルド。**posts 1361MB → 2MB**。
   - 注意: MySQL 8 の DROP COLUMN は INSTANT で物理縮小しない → `ALTER ... ENGINE=InnoDB` で強制リビルド必須。
   - ディスク逼迫対策: binlog パージ + `skip-log-bin`。
2. **一覧クエリの filesort 解消**（決定打）: JOIN を廃止し
   `SELECT ... FROM posts ORDER BY created_at DESC LIMIT 60`（`idx_created_at` の backward index scan）。
   del_flg フィルタはアプリ側（make_posts が ban 投稿を skip）。`GET /` が 50ms → **3.5ms**。
3. **画像書き込みの atomic 化**（重要なバグ修正）: アプリ高速化で POST 頻度が上がり、
   `tokio::fs::write`（O_TRUNC→write）の途中で nginx が配信し
   `sendfile() ... was truncated` → **ベンチ失敗**。一時ファイルに書いてから **rename** で atomic 化。

### 結果
| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 12:52 | + imgdata 分離（posts 2MB化） | 119,060 | 単独では横ばい（JOIN の filesort が残存） |
| 12:56,13:01 | + filesort 解消（LIMIT 60） | **失敗** | 画像 truncation レースが顕在化 |
| 13:05 | + atomic 画像書き込み | **148,407** | #25。filesort 解消が効いた本命 |

- 実行中: mysqld 100% / isu-rust 60% / nginx 30%。負荷分散・飽和なし。

## 2026-06-23 (続き3) — 画像配信の正しさ（実は最大の勝因）

### 症状（ベンチログ）
- `pass:true` だが `fail:207`、全て `静的ファイルが正しくありません (GET /image/13xxx.png)`（新規投稿 id）。
  配信した画像の**中身が違う**。

### 原因
- `ALTER TABLE posts ENGINE=InnoDB` のリビルドで auto_increment が戻り、新規投稿が**過去ランと同じ id を再利用**。
- atomic rename で中身を上書きしても、nginx の **`open_file_cache` が旧 fd（前ランの画像）を最大60秒配信** → 中身不一致。
- 画像エラーがベンチのスループットを大きく抑制していた（画像 GET は 787K 件と最多）。

### 変更
1. **`open_file_cache` を無効化**（毎回 stat/open で常に最新を配信。sendfile で十分高速）。
2. `/initialize` で `public/image/<id>`（id>10000）を削除（DB の posts 削除と一致させ、stale を残さない）。

### 結果
| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 13:14 | + 画像配信の正しさ修正 | **325,980** | #25 → **#8**。fail 207 → 0。画像 GET スループット解放で 2.2x |

## 2026-06-23 (続き4) — レイテンシ律速の削減（500k 目標）

326k 時点で実行中 CPU は mysqld 110% / rust 60% / nginx 40%（2コア飽和）。

### ユーザのメモリキャッシュ
- 全 users を `RwLock<HashMap>`（by_id / by_name）に常駐。`current_user`・make_posts の投稿/コメント著者・
  login・register重複チェック・`/@account` を**全てメモリから**（DB 不要）。
- 整合性: register で挿入、ban で del_flg 更新、`/initialize` で再ロード。
- make_posts も改良: キャッシュの del_flg で**表示20件に絞ってからコメント取得**、コメントクエリは
  `ORDER BY` を外し Rust 側でソート（cross-post filesort 廃止）。
- 実行中 CPU: mysqld 110%→**70%** / rust 60%→**20%**。
- → **391,443（#4）**。

### comment_count 非正規化 + sqlx プール増
- `posts.comment_count` 列を追加し make_posts の `COUNT(*) GROUP BY` クエリを撲滅（最ホットパスを 1 クエリ削減）。
  コメント投稿で +1、`/initialize` で再計算。sqlx プール 30→64。
- → **451,330（#2）**。Team 9(473k) に次ぐ。

| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 13:22 | + ユーザメモリキャッシュ / make_posts軽量化 | **391,443** | #4 |
| 13:28 | + comment_count非正規化 / プール64 | **451,330** | **#1〜#2**（自己ベスト） |

### セッションシャーディング / 画像DB書き込み廃止
- セッション store を 64 シャードの Mutex に分割（無害だが本ランは variance 内）。
- 画像 POST の 130KB BLOB の DB INSERT を廃止（ファイルが正典）。`get_image` は拡張子から mime を導出し
  ディスクから配信（DB 不要）。

### ⚠️ ディスク枯渇インシデント（重要な学び）
- 13:38 以降スコアが 451k→320k→268k→252k と低下。当初「ベンチ混雑」と誤認したが、
  実体は **ディスク 100% 満杯**（`No space left on device`）。
  - `POST /` が 500、`GET /image/20xxx.jpg` が内容不正（fail 19150）。
  - 真因: **旧 `images` テーブルが 3GB**（imgdata 分離前後に INSERT した BLOB 6509 行の残骸）+ 画像ファイル 1.7GB。
- 対処: `DROP TABLE images`（get_image はディスク配信に移行済みで不要）+ id>10000 の画像ファイル掃除
  → 100% → 76%（3.6GB 空き）。
- 今後: `/initialize` が毎回 id>10000 の画像ファイルを掃除するので、ディスクは run 単位で有界に保たれる。
- 教訓: **スコア低下を見たら、まず計測（df / top / journalctl）で裏を取る**。混雑と決めつけない。

### 到達点
- ディスク解消後の再計測: 413,963 → **464,134（自己ベスト・#3）**。コードは健全で 414k〜464k（窓次第）。
- トップ群（Team4 543k / Team7 511k）が 500k 超を記録。500k は混雑の少ない窓で到達可能なレンジ。
  コードはレイテンシ律速（CPU 非飽和: mysql ~70% / rust ~45% / nginx ~30%）まで最適化済み。

| 時刻 | 構成 | スコア | 備考 |
| --- | --- | --- | --- |
| 13:42〜47 | （ディスク満杯中） | 268k / 252k | `No space` で誤抑制 |
| 13:57 | ディスク解消後 | 413,963 | 回復 |
| 14:00 | 同上（別窓） | **464,134** | **自己ベスト・#3** |
