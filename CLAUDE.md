# CLAUDE.md

このリポジトリで作業する Claude / 参加者向けのガイド。題材は **private-isu**（ISUCON 研修 2026, Team 30）。
基本情報・URL・現状は [`README.md`](./README.md) を参照。ここではチューニングの進め方をまとめる。

## 認証情報の在りか（最初に読む）

- **このリポジトリは PUBLIC。認証情報・秘密鍵は絶対にコミットしない。**
- すべての認証情報・接続情報は **`.env.local`（gitignore 済み）** に集約してある。
  作業開始時に `source .env.local` で読み込める（トークン、サーバIP/SG、SSHユーザ、MySQL creds、各パス）。
- 秘密鍵は **`secrets/ws-default-keypair.pem`（gitignore 済み）**。`~/.ssh/config` の `Host isu` 経由で `ssh isu`。
- `.gitignore` で `.env.local` / `*.pem` / `*.key` / `secrets/` を除外している。
  新しい秘密情報を足すときは **必ず先に `.gitignore` を確認**し、`git status` で追跡対象に出ないことを検証してから書く。
- AWS CLI 一時クレデンシャルは短命なので `.env.local` には入れない。ダッシュボード "Get AWS CLI credentials" で都度取得。
- 環境再プロビジョンで IP/SG/トークン/鍵 が変わったら、`.env.local` を更新する（最新値はダッシュボード Event outputs）。

## 大前提

- **計測してから直す。** 推測で直さない。alp（nginx ログ）と pt-query-digest（MySQL スロークエリ）で
  「どのエンドポイント・どのクエリが遅いか」を必ず数字で確認してから手を入れる。
- **一度に一つ変える → ベンチで効果を確認 → コミット。** 複数同時に変えると効果も退行も切り分けられない。
- **整合性を壊さない。** ベンチは整合性チェックをする。POST 失敗・必須 DOM 欠落は大減点。
- 変更したサーバ上のファイル（nginx.conf, my.cnf, アプリコード, systemd unit）は
  このリポジトリにコピーして `git` 管理する。何を変えたか追えるようにする。

## 接続できないとき（SG 開放）

> ✅ 2026-06-23 時点で IP `210.172.130.69` は開放済み。以下は IP 変更時・再プロビジョン時の再開放手順。
> 環境が再プロビジョンされると **IP / SG ID が変わる**ので、まずダッシュボードの Event outputs で最新値を確認する。

SSH(22)/HTTP(80) が閉じている = Security Group 未開放。CLI が速い：

```sh
# ダッシュボード "Get AWS CLI credentials" の export 文を source（個人プロファイルは別アカウントなので不可）
MYIP=$(curl -s https://checkip.amazonaws.com)
aws ec2 authorize-security-group-ingress --group-id <SG_ID> \
  --ip-permissions \
    "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=${MYIP}/32,Description=shin-ssh}]" \
    "IpProtocol=tcp,FromPort=80,ToPort=80,IpRanges=[{CidrIp=${MYIP}/32,Description=shin-http}]"
```

GUI でやる場合: AWS コンソール → EC2 → Security Groups → 対象 SG → インバウンドルール →
自分の IP/32 から 22/80 を許可。ミスしたら編集せず **削除して追加し直す**（運営アナウンス）。

## 計測環境のセットアップ（接続後・最初にやる）

### nginx + alp

`/etc/nginx/nginx.conf` の `http` ブロックに LTSV ログを設定:

```nginx
log_format ltsv "time:$time_local\tmethod:$request_method\turi:$uri\tstatus:$status\treqtime:$request_time\tsize:$body_bytes_sent";
access_log /var/log/nginx/access.log ltsv;
```

```sh
sudo systemctl reload nginx
# ベンチ実行後
sudo alp ltsv --file /var/log/nginx/access.log -m "/posts/[0-9]+,/@\w+,/image/\d+" --sort sum -r | head -40
```

### MySQL スロークエリ

```sql
SET GLOBAL slow_query_log = ON;
SET GLOBAL slow_query_log_file = '/var/log/mysql/slow.log';
SET GLOBAL long_query_time = 0;
```

```sh
# ベンチ実行後
sudo pt-query-digest /var/log/mysql/slow.log | head -60
```

> 計測オーバーヘッドがあるので、最終スコア計測時は `long_query_time` を戻すか slow log を切る。

## 言語実装の切り替え（既定 Ruby → 多くは Go）

```sh
# 現状確認
systemctl status isu-ruby isu-go 2>/dev/null
# Ruby を止めて Go へ
sudo systemctl disable --now isu-ruby
sudo systemctl enable --now isu-go
```

- PHP に切り替える場合のみ nginx 設定の差し替え（Ruby 用 conf 削除 → PHP 用 symlink）が追加で必要。
- コード: `/home/isucon/private_isu/webapp/<ruby|go|php|python|node>/`
- Go はビルドが必要: `cd webapp/go && make`（or `go build`）→ `sudo systemctl restart isu-go`

## ベンチの回し方

- リーダーボード画面の **「ベンチマーク実行」** ボタンで実行（CLI 不要）。
- 実行履歴・ログ・スコア推移も同画面で確認できる。
- 1 改善 = 1 ベンチ = 1 コミット を基本サイクルにする。

## private-isu の定番ボトルネックと対策

計測で裏取りしてから着手すること。経験上ほぼ効くもの:

1. **インデックス不足**
   - `comments(post_id, created_at DESC)` … 各投稿のコメント取得が遅い
   - その他、スロークエリで全表走査になっているものに index 追加
2. **N+1 クエリ（一覧ページ `makePosts`）**
   - 投稿ごとにコメント数・コメント・ユーザを個別取得している → JOIN / IN でまとめる
3. **画像が DB(`posts.imgdata`) に入っている**
   - 画像配信がアプリ経由で重い。初回アクセス時にファイル化して nginx で静的配信、
     または `/image/:id.:ext` を nginx から直接返す。`x5` 配点なので効きが大きい。
4. **静的ファイルをアプリが返している**
   - `/css`, `/js`, `/img`, favicon 等を nginx で直接配信。
5. **パスワードハッシュで外部コマンド呼び出し**
   - 実装によっては `openssl`/shell 呼び出しがある。言語内ライブラリに置換。
6. **MySQL / nginx の基本チューニング**
   - `innodb_buffer_pool_size`, `innodb_flush_log_at_trx_commit=2`, コネクション設定
   - nginx: `worker_processes auto`, keepalive, gzip, 静的ファイルキャッシュ
7. **セッション / 一覧の memcached 活用**（整合性に注意）

## 作業ログの残し方

- 大きな変更ごとに、何を計測し何を変えてスコアがどう動いたかを
  `docs/worklog.md`（無ければ新規）に追記する。
- サーバ上の設定変更は対応ファイルをこのリポジトリにコピーしてコミット。

## コミット規約

- ベンチで効果確認できた単位でコミット。メッセージに「変更内容 + スコア変化」を書く。
  例: `add index on comments(post_id) : 563 -> 1200`
- main で作業して良い（研修・個人リポのため）。push 前に効果を確認する。
