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
| サーバ Public IP | `54.238.112.154` |
| Security Group ID | `sg-03134b65bb78ea785` |
| リージョン | ap-northeast-1 |

### 各種URL

- AWS環境（ワンタイムパスワード認証 / 会社メール）: https://catalog.us-east-1.prod.workshops.aws/join
- リーダーボード（ベンチマーク実行）: https://is-b1a9ac8b2a4c42b985d2104d456a970d.ecs.ap-northeast-1.on.aws/
- マニュアル: https://github.com/catatsuy/private-isu/blob/master/manual.md

## 現状（2026-06-23 時点）

- このリポジトリはほぼ空（プレースホルダのみ）。アプリ本体は EC2 サーバ上にある。
- **⚠️ まだ SSH(22) / HTTP(80) ともにサーバへ接続できない。**
  Security Group に自分のグローバル IP が許可されていないため。
  - 自分のグローバル IP は `curl https://checkip.amazonaws.com` で確認（作業時点では `210.172.130.69`）。
  - AWS コンソールで SG `sg-03134b65bb78ea785` のインバウンドルールに、
    自分の IP からの **22(SSH)** と **80(HTTP)** を許可する。
  - 間違ったルールを追加した場合は「編集」せず、**削除 → 追加** し直す（運営アナウンス）。

## はじめに（接続後の流れ）

1. SG に自分の IP を許可 → `ssh isucon@54.238.112.154`（EC2 SSH キーはワークショップの "Get EC2 SSH key" から取得）
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
