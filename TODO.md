# TODO — private-isu チューニング (Team 30)

最終更新: 2026-06-23 / **自己ベスト 464,134（順位 #3）** / 稼働言語 **Rust (`isu-rust`)**

進め方の原則は [`CLAUDE.md`](./CLAUDE.md)、結果の推移は [`docs/worklog.md`](./docs/worklog.md) を参照。
**計測 → 1改善 → ベンチ → コミット** を1サイクルにする。

---

## 📈 スコア推移（このセッション）

| 構成 | スコア | 順位 |
| --- | --- | --- |
| Ruby（既定） | 566 | — |
| Rust + インデックス | 34,416 | #18 |
| + 画像 nginx 直配信 / 静的直配信 / MySQL・nginx チューニング | 57,371 | #22 |
| + make_posts の N+1 解消（バッチ取得） | 123,131 | #17 |
| + imgdata 分離 / 一覧クエリの filesort 解消 / atomic 画像書き込み | 148,407 | #25 |
| + 画像配信の正しさ修正（open_file_cache 無効化） | 325,980 | #8 |
| + ユーザのメモリキャッシュ / make_posts 軽量化 | 391,443 | #4 |
| + comment_count 非正規化 / sqlx プール64 | 451,330 | #2 |
| + セッション分割 / 画像 DB 書き込み廃止（ディスク解消後の再計測） | **464,134** | **#3** |

→ ベースライン比 **約 820x**。

---

## ✅ 完了

- [x] SG 開放 / SSH 接続 / サーバ状況把握
- [x] **Rust 全面書き直し**（axum + sqlx + askama、Go 参照実装を正典に全14ルート移植）
- [x] インデックス追加（`db/indexes.sql`）
- [x] **計測環境**: alp(v1.0.21) + nginx LTSV ログ / MySQL slow query（必要時のみ ON）/ perf_schema
- [x] **画像を nginx 直配信**: `posts.imgdata` をファイル化（`public/image/<id>.<ext>`）、nginx `try_files`。
      初回 GET でファイル化 + POST 投稿時もファイル化。全画像 pre-warm 済み。
- [x] **静的ファイル直配信** + nginx チューニング（worker, upstream keepalive, gzip, tcp_nodelay）
- [x] **MySQL チューニング**（`etc/mysql/mysql.conf.d/zz-isucon.cnf`）: buffer pool, `flush_log_at_trx_commit=2`,
      `O_DIRECT`, redo 256M, `skip-log-bin`
- [x] **make_posts の N+1 解消**: 投稿ごとの個別クエリ群 → IN クエリにバッチ集約。一覧は `LIMIT` + アプリ側 del_flg フィルタ。
- [x] **imgdata を posts から分離**（`db/split_imgdata.sql`）: posts を 1361MB → 2MB に縮小（PKルックアップ高速化）。
      ※ MySQL8 の DROP COLUMN は INSTANT なので `ALTER ... ENGINE=InnoDB` で物理リビルド必須。
- [x] **一覧クエリの filesort 解消**: JOIN 廃止 → `ORDER BY created_at DESC LIMIT N`（`idx_created_at` の backward index scan）。
- [x] **画像書き込みの atomic 化**（tmp→rename）+ **open_file_cache 無効化**（旧ランの画像を配信する不整合の修正）。
      `/initialize` で id>10000 の画像ファイルを掃除。
- [x] **ユーザのメモリキャッシュ**（`RwLock<HashMap>`）: current_user / make_posts の著者 / login / register / `/@account` を DB 非依存に。
      register 挿入・ban 更新・`/initialize` 再ロードで整合性維持。
- [x] **comment_count 非正規化**（`db/comment_count.sql`）: GROUP BY COUNT クエリを撲滅。投稿で +1、`/initialize` で再計算。
- [x] **セッション store を 64 シャードに分割** / sqlx プール 64
- [x] **画像 POST の DB BLOB 書き込み廃止**（ファイルが正典）。`get_image` は拡張子→mime 導出 + ディスク配信（DB 不要）。
- [x] **ディスク枯渇インシデント対処**: 旧 `images`(BLOB) テーブル 3GB を DROP（ディスク 100%→76%）。詳細は worklog 参照。

---

## 🔜 残タスク（50万到達のため）

> 現状はレイテンシ律速（実行中 CPU: mysql ~70% / rust ~45% / nginx ~30% で**非飽和**）。
> スコアは 414k〜464k で、変動は**共有ベンチマーカーの混雑**由来。トップ群（Team4 543k / Team7 511k）は到達済みなので 500k は射程内。

### 1. GET / ・GET /posts のレイテンシ削減（本命・要検討）
- [ ] **一覧データ（最新20投稿+コメント+著者）をメモリキャッシュ**し、GET / の DB 往復(2クエリ)を 0 にする。
      POST / ・POST /comment ・POST /admin/banned で無効化（lazy 再構築）。
      ⚠️ 整合性が崩れるとベンチ全体失敗（fail 多発で減点）なので、無効化タイミングを厳密に。要A/B計測。
- [ ] コメントの list 取得を window 関数（`ROW_NUMBER() ... rn<=3`）にして転送量削減（低リスク・小幅）。
- [ ] `POSTS_FETCH_LIMIT`(現60) を 30〜40 に（一覧クエリの PK ルックアップ/転送を削減・小幅）。

### 2. 混雑の少ない窓での再計測
- [ ] セッション終盤など他チームのベンチが減る時間帯に再実行（同一コードで 500k+ の可能性）。

### 3. その他・検討
- [ ] 画像 POST のファイル書き込みを応答前に待つ→非同期化（x5 パスのレイテンシ）。ただし直後 GET との競合に注意。
- [ ] MySQL がほぼ 1 コアしか使えていない場合のコネクション/並列度の再点検（現状はレイテンシ律速で優先度低）。
- [ ] 最終計測時は slow log / alp ログを切る（オーバーヘッド除去）。

---

## ⚠️ 運用上の重要注意

- **ディスクが 15GB と小さい**（OS 等で既に ~8.6GB 使用）。画像ファイル(public/image)+MySQL でひっ迫しやすい。
  - `/initialize` が id>10000 の画像ファイルを掃除するので、画像は run 単位で有界。
  - 旧 `images`(BLOB) テーブルは **DROP 済み**（`get_image` はディスク配信が正典）。再作成しないこと。
  - **スコアが急落したら、まず `df -h /` / `top` / `journalctl -u isu-rust` で裏取り**（混雑と決めつけない）。今回 451k→252k 急落の真因はディスク 100% だった。
- ベンチは実行間隔・同時実行に制限あり。連打しても新規ランは登録されない（前のランが in-flight 中は no-op）。
- 環境再プロビジョンで **IP / SG が変わる**。最新値はダッシュボード Event outputs（手順は CLAUDE.md）。
- `/initialize` は 10 秒以内（現状 ~0.25s）。インデックス・スキーマ変更・ファイル化画像(id≤10000)は残る。

---

## 📂 リポジトリ内の成果物

- `webapp/rust/src/main.rs` — Rust 実装本体（全最適化込み）
- `db/indexes.sql` / `db/split_imgdata.sql` / `db/comment_count.sql` — スキーマ変更（適用済み）
- `etc/nginx/nginx.conf` / `etc/nginx/sites-available/isucon.conf` — nginx 設定（適用済み）
- `etc/mysql/mysql.conf.d/zz-isucon.cnf` — MySQL 設定（適用済み）
- `deploy/isu-rust.service` — systemd unit
- `docs/worklog.md` — 計測・改善・スコアの時系列ログ
