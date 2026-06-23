# isucon-shin

日本CTO協会 合同ISUCON研修 2026 の作業リポジトリ（参加者: リョウシン）。

題材は [private-isu](https://github.com/catatsuy/private-isu)（SNS風画像投稿アプリ「Iscogram」）。
このリポジトリは、サーバ上の設定ファイル・アプリコード・チューニング作業をバージョン管理し、
作業ログを残すために使う。

## 参加情報

| 項目 | 値 |
| --- | --- |
| チーム | **Team 30** |
| ベンチマークトークン | `38c5-11f5cd-5e` |
| サーバ Public IP | `54.95.55.129` |
| Security Group ID | `sg-0143afb4d92447bee` |
| AWS アカウント（ワークショップ） | `251937262269` |
| リージョン | ap-northeast-1 |

> ⚠️ 環境が再プロビジョニングされると IP / SG ID が変わる。当初連絡値（IP `54.238.112.154` / SG `sg-03134b65bb78ea785`）は失効済み。最新値は Workshop Studio ダッシュボードの **Event outputs** で確認すること。

### 各種URL

- AWS環境（ワンタイムパスワード認証 / 会社メール）: https://catalog.us-east-1.prod.workshops.aws/join
- リーダーボード（ベンチマーク実行）: https://is-b1a9ac8b2a4c42b985d2104d456a970d.ecs.ap-northeast-1.on.aws/
- マニュアル: https://github.com/catatsuy/private-isu/blob/master/manual.md

## 現状（2026-06-23 時点）

- このリポジトリはほぼ空（プレースホルダのみ）。アプリ本体は EC2 サーバ上にある。
- **✅ SSH(22) / HTTP(80) 開通済み。** SG `sg-0143afb4d92447bee` に自分の IP `210.172.130.69/32` を許可した
  （ルール `sgr-02a85aa889bf958cf` = SSH, `sgr-05cd63566410e7074` = HTTP）。
  `http://54.95.55.129/` は HTTP 200 を返す（応答 ~1.6s でチューニング余地大）。
- **次のステップ:** ダッシュボードの **"Get EC2 SSH key"** で秘密鍵を取得して SSH 接続。

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
