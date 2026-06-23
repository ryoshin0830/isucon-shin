# isucon-shin

日本CTO協会 合同ISUCON研修 2026 の作業リポジトリ（参加者: リョウシン）。

題材は [private-isu](https://github.com/catatsuy/private-isu)（SNS風画像投稿アプリ「Iscogram」）。
このリポジトリは、サーバ上の設定ファイル・アプリコード・チューニング作業をバージョン管理し、
作業ログを残すために使う。

## 参加情報

| 項目 | 値 |
| --- | --- |
| チーム | **Team 30** |
| ベンチマークトークン | → `.env.local`（`TEAM_ACCESS_CODE`）※PUBLICリポのため非掲載 |
| サーバ Public IP | `54.95.55.129` |
| Security Group ID | `sg-0143afb4d92447bee` |
| AWS アカウント（ワークショップ） | `251937262269` |
| リージョン | ap-northeast-1 |

> ⚠️ 環境が再プロビジョニングされると IP / SG ID が変わる。当初連絡値（IP `54.238.112.154` / SG `sg-03134b65bb78ea785`）は失効済み。最新値は Workshop Studio ダッシュボードの **Event outputs** で確認すること。

### 各種URL

- AWS環境（ワンタイムパスワード認証 / 会社メール）: https://catalog.us-east-1.prod.workshops.aws/join
- リーダーボード（ベンチマーク実行）: https://is-b1a9ac8b2a4c42b985d2104d456a970d.ecs.ap-northeast-1.on.aws/
- マニュアル: https://github.com/catatsuy/private-isu/blob/master/manual.md

### 認証情報（`.env.local`）

> ⚠️ **このリポジトリは PUBLIC**。認証情報は **`.env.local`（gitignore 済み・コミット禁止）** に集約している。

`.env.local` に以下を保存している（`source .env.local` で環境変数として読み込み可能）:

- リーダーボードのアクセスコード（ベンチマークトークン）
- AWS ワークショップ情報（アカウントID / リージョン / ダッシュボードURL）
- サーバ接続情報（Public IP / インスタンスID / SG ID / SSHユーザ）
- MySQL 接続情報（user/pass/db）
- アプリのパス等

秘密鍵は **`secrets/ws-default-keypair.pem`（gitignore 済み）** に置き、`~/.ssh/config` の `Host isu`
エイリアスから参照している（`ssh isu` で接続）。

> AWS CLI の一時クレデンシャル（`ASIA…`/session token）は短命なので保存していない。
> 必要時にダッシュボード **"Get AWS CLI credentials"** の export 文を貼り付けて使う。

## 現状（2026-06-23 時点）

- このリポジトリはほぼ空（プレースホルダのみ）。アプリ本体は EC2 サーバ上にある。
- **✅ SSH(22) / HTTP(80) 開通済み。** SG `sg-0143afb4d92447bee` に自分の IP `210.172.130.69/32` を許可した
  （ルール `sgr-02a85aa889bf958cf` = SSH, `sgr-05cd63566410e7074` = HTTP）。
  `http://54.95.55.129/` は HTTP 200 を返す（応答 ~1.6s でチューニング余地大）。
- **✅ SSH 接続済み。** 秘密鍵 `~/Downloads/ws-default-keypair.pem` を取得し、`ssh isu`（`~/.ssh/config` にエイリアス登録）で接続可能。
  - 接続先: `isucon@54.95.55.129`（Ubuntu 24.04 / kernel 6.17 / **2 vCPU / 3.8GB RAM**）
  - ⚠️ CLI 経由の鍵取得は不可だった（SSM 非管理 / Parameter Store に鍵なし / EC2 Instance Connect 権限なし）。鍵はダッシュボード **"Get EC2 SSH key"** からのみ取得できる。
- **サーバ現状:** **Rust 実装(`isu-rust`)が稼働中**。Ruby は停止・無効化。nginx・mysql active。
- **スコア:** Ruby 566 → … → **自己ベスト 464,134（順位 #3）**。一連の改善で約 820x。詳細は [`docs/worklog.md`](./docs/worklog.md)。
  - 効いた改善（順に）: 索引追加 → 画像のファイル化+nginx直配信 → make_posts の N+1 解消 → imgdata 分離+一覧クエリの filesort 解消 → 画像配信の正しさ修正（open_file_cache 無効化）→ ユーザのメモリキャッシュ → comment_count 非正規化 → セッション分割。
  - 現状はレイテンシ律速（CPU 非飽和: mysql ~70% / rust ~45% / nginx ~30%）。スコアは 414k〜464k で、変動は**共有ベンチマーカーの混雑**由来。
  - **500k 目標**: トップ群（Team4 543k / Team7 511k）が到達済みで射程内。混雑の少ない窓での再計測 or さらなるレイテンシ削減（GET / の往復削減 = 一覧データのメモリキャッシュ等）が残課題。
- **運用注意（重要）:** ディスク 15GB と小さい。画像ファイル(public/image)+MySQL でひっ迫しやすい。
  `/initialize` が id>10000 の画像ファイルを掃除する。旧 `images`(BLOB) テーブルは DROP 済み（ファイル配信が正典）。

### SG 開放のやり方（IP が変わった / 再プロビジョン時の再開放）

ダッシュボードの **"Get AWS CLI credentials"** で一時認証情報（`export AWS_...` の bash 形式）を取得し、
シェルに読み込んでから CLI で追加するのが速い。**個人プロファイル（`default` 等）はワークショップとは別アカウントなので使えない。**

```sh
# 取得した export 文を貼り付け or source した後で
MYIP=$(curl -s https://checkip.amazonaws.com)
aws ec2 authorize-security-group-ingress --group-id sg-0143afb4d92447bee \
  --ip-permissions \
    "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=${MYIP}/32,Description=shin-ssh}]" \
    "IpProtocol=tcp,FromPort=80,ToPort=80,IpRanges=[{CidrIp=${MYIP}/32,Description=shin-http}]"
```

> コンソール GUI でやる場合：間違ったルールは「編集」せず **削除 → 追加** し直す（運営アナウンス）。

## はじめに（接続後の流れ）

1. ~~SG に自分の IP を許可~~ ✅ 完了 → `ssh -i <鍵> isucon@54.95.55.129`（鍵はダッシュボード "Get EC2 SSH key"）
2. アプリの言語実装を確認・切り替え（既定は Ruby、本研修では Go へ切り替えることが多い）
3. 計測環境を整備（nginx の alp 用ログ、MySQL スロークエリログ）
4. ベンチを回してボトルネックを計測 → 改善 → 再計測 を繰り返す

詳しい作業手順・サーバ構成・チューニング方針は [`CLAUDE.md`](./CLAUDE.md) を参照。

## サーバ構成（private-isu）

```
[Benchmarker] --> nginx:80 --> app(Unicorn/PHP-FPM/Go/Python/Node) --> MySQL:3306
                                                                  \-> memcached:11211
```

| 役割 | 詳細 |
| --- | --- |
| Web | nginx (port 80) |
| App | systemd: `isu-ruby` / `php8.3-fpm` / `isu-go` / `isu-python` / `isu-node` |
| DB | MySQL (port 3306) — user `isuconp` / pass `isuconp` / db `isuconp` |
| Cache | memcached (port 11211) |
| アプリコード | `/home/isucon/private_isu/webapp/<言語>/` |

## スコアリング

```
成功GETレスポンス数 x1 + 成功POST数 x2 + 画像投稿成功数 x5 − (エラー/例外/失敗のペナルティ)
```

- `GET /initialize` は 10 秒以内に完了する必要がある。
- レスポンス HTML に必須の DOM 要素が含まれていないと減点。
- POST 失敗は大きな減点。整合性を壊さないこと。
