# TODO — private-isu チューニング (Team 30)

最終更新: 2026-06-23 / 現在スコア **34,416**（順位 #18）/ 稼働言語 **Rust (`isu-rust`)**

進め方の原則は [`CLAUDE.md`](./CLAUDE.md)、結果の推移は [`docs/worklog.md`](./docs/worklog.md) を参照。
**計測 → 1改善 → ベンチ → コミット** を1サイクルにする。

---

## ✅ 完了

- [x] SG 開放（自IP `210.172.130.69` に SSH/HTTP 許可。CLI 一時認証経由）
- [x] SSH 接続（鍵DL → `ssh isu` エイリアス）
- [x] サーバ状況把握（Ubuntu 24.04 / 2 vCPU / 3.8GB、Ruby稼働、1000/10000/100000行）
- [x] **Rust 全面書き直し**（axum + sqlx + askama、Go参照実装を正典に全14ルート移植）
  - [x] passhash 厳密再現（openssl計算値とDB値一致を実測）
  - [x] セッション / CSRF / multipart投稿 / 画像配信 / `/@account`
  - [x] sqlx の TIMESTAMP decode 問題を `DateTime<Utc>` + `time_zone='+00:00'` で解決
- [x] デプロイ（rustup導入 → build → systemd `isu-rust`、`isu-ruby` 停止）
- [x] 整合性確認（ベンチ `pass: true`）
- [x] **インデックス追加**（`db/indexes.sql`）→ 566 → 0(timeout) → **34,416**

---

## 🔜 残タスク（おすすめ順）

> まず計測環境を入れて、数値でボトルネックを確認してから着手する。

### 1. 計測環境の整備（最優先・着手前提）
- [ ] alp 導入（nginx LTSVログ設定 → reload）。`-m` で `/posts/\d+,/@\w+,/image/\d+` を集約
- [ ] pt-query-digest 導入（MySQL slow_query_log ON, long_query_time=0）
- [ ] 1回ベンチを回して「どのエンドポイント/クエリが重いか」を数値化
- [ ] 設定変更ファイルは都度リポジトリにコピーして管理（nginx.conf, my.cnf 等）

### 2. GET / の N+1 解消（索引の次に効く王道）
- [ ] `makePosts` の N+1 を解消。投稿一覧に対しコメント数・コメント・ユーザを
      `IN` / `JOIN` でまとめ取得（現状は Go 同様、投稿ごとに個別クエリ）
- [ ] コメント数は `comments` を `post_id` で集計して一括取得 or キャッシュ
- [ ] `SELECT * FROM posts ORDER BY created_at DESC`（全件）に `LIMIT` を入れて
      過剰フェッチを抑える（del_flgユーザ除外で20件確保できる十分なバッファで）

### 3. 画像配信の最適化（画像投稿は x5 配点・効果大）
- [ ] `posts.imgdata`(DB) を初回アクセス時にファイル化、`/image/:id.:ext` を nginx 直配信
- [ ] 投稿時(POST /)にもファイル書き出し
- [ ] nginx に `try_files` / `location /image/` を設定

### 4. 静的ファイルを nginx 直配信
- [ ] `/css` `/js` `/img` `/favicon.ico` を nginx の `root` から直接返す
      （現状 Rust アプリの ServeDir フォールバックが返している）

### 5. ミドルウェアのチューニング
- [ ] MySQL `innodb_buffer_pool_size`（3.8GBに合わせ ~1〜2GB）, `innodb_flush_log_at_trx_commit=2`
- [ ] nginx `worker_processes auto`, keepalive, gzip
- [ ] sqlx コネクションプール上限の調整（現状 max 30）

### 6. その他・検討
- [ ] セッションが現状プロセス内メモリ（単一プロセス前提）。複数プロセス化するなら memcached/Redis へ
- [ ] `posts.imgdata` を別テーブル/ストレージへ分離して `SELECT *` を軽量化
- [ ] 最終計測時は slow log / alp ログを切ってオーバーヘッド除去

---

## メモ / 既知の注意
- 環境再プロビジョンで **IP / SG が変わる**。最新値はダッシュボード Event outputs を確認（手順は CLAUDE.md）。
- `/initialize` は行の DELETE/UPDATE のみ。**インデックスやファイル化した画像は残る**。
- 変更は1つずつ。ベンチで効果確認してからコミット（メッセージにスコア変化を記録）。
