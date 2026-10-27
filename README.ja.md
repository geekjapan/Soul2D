# Soul2D

[English](README.md) | 日本語

> この文書は [README.md](README.md)（英語）の日本語訳です。正本は英語版です。内容が食い違う場合は英語版に従ってください。

Soul2D は、Rust 製の編集コアを持つ実験的な 2D モデルエディタです。
旧名は VSoul25D です。実行ファイルは `soul2d-core`（ヘッドレス CLI と MCP ホスト）、
`njc`（MCP クライアント）、`soul2d-editor`（実験的な GUI）の 3 つです。
C ABI のシンボル、MCP のリソース識別子、旧設定ディレクトリは互換性のため以前の名前のままです。
[nijigenerate](https://github.com/nijigenerate/nijigenerate) と
[Inochi2D](https://github.com/Inochi2D/inochi-creator) から引き継いだコードとワークフローを発展させています。

**開発状況：未完成です。バイナリ配布は停止しています。**
このリポジトリはソースの閲覧と開発のために公開しています。サポート対象のリリース、インストーラー、
Nightly ビルド、Flatpak 配布はありません。
このリポジトリは新しい Git 履歴から始まります。それ以前の開発履歴は公開していません。

## 現在の範囲

Rust の Session が、ドキュメントの状態、リビジョン、undo/redo、保存を管理します。
クライアントは JSONL CLI、stdio と HTTP の MCP、`njc` MCP クライアント、ACP エージェントパネルを備えた
実験的な Rust GUI です。以前の D アプリケーションは削除済みです。
[移行マップ](doc/rust-module-migration-map.md)に、各 D モジュールの置き換え先と残っている不足点をまとめています。

現在の作業対象は、INP/INX の編集、シーンのプレビュー、パラメータとデフォーマの操作、
CMO3/CAN3 ソースファイルの限定的な読み取りと編集です。
対応状況は形式と操作ごとに異なります。ソースファイルを読み書きできても、
他のエディタで正しく描画・再生できることは保証されません。

CAN3 の Outline で Image または Guide トラックと読み取り可能なスカラーチャンネルを展開すると、
キーの追加・削除、アンカー値・左右のハンドル・保存された補間種別の編集ができます。
保存前に各編集を適用または破棄してください。存在しないチャンネルや固定値チャンネルは作成しません。

次の作業は未完成です。

- パッケージング：`build-aux/osx/BuildUniversal.sh` は Rust バイナリから署名なしの macOS アプリバンドルを作ります
  （arm64 ホストで universal `x86_64 arm64` ビルドを実行。`x86_64` スライスは Rosetta での起動確認のみ。DMG は作成と
  マウントを確認済み）。Flatpak マニフェストと WiX プロジェクトは `.github/workflows/packaging.yml` でのみビルドされます。
  どちらのパッケージもユーザーのマシンにインストールしたことはなく、署名もしていません。[BUILD.md](BUILD.md) を参照してください。
- Rust エディタの実操作による検証。macOS での作成・undo・保存・再読み込みの 1 フロー以外は未確認です。
- Live2D Cubism と同等の編集、描画、アニメーション、物理演算、ポーズ。
- 表示情報と物理設定以外の Live2D ランタイムデータの書き出し。`live2d.cdi3.export` は `.cdi3.json` を、
  `live2d.physics3.export` は `.physics3.json` を書き出しますが、`.model3.json`、`.pose3.json`、`.exp3.json`、
  `.moc3` は書き出しません。
- ArtPath のネイティブな評価・描画、Glue の作成と編集、
  サンプルで計測できなかったデフォーマのケース。保存された ArtPath 制御点は編集できますが、
  ネイティブな描画への対応を示すものではありません（[doc/live2d-authoring.md](doc/live2d-authoring.md) を参照）。
- Windows、macOS、および Metal、CUDA、Vulkan、ROCm などの GPU バックエンドの実運用レベルの検証。

実験にはモデルのコピーを使ってください。

## ドキュメント

- [チュートリアル](user-doc/tutorial/ja/01-model-structuring.md)：モデルの構成と
  [パラメータの定義](user-doc/tutorial/ja/02-parameter-definition.md)。
- [ソースからのビルド](BUILD.md)と[エディタの翻訳](TRANSLATING.md)（英語）。
- [開発者向けドキュメントの索引](doc/README.md)（英語）：[アーキテクチャ](doc/ARCHITECTURE.md)、
  [MCP API](doc/mcp_api.md)、[形式変換マップ](doc/format-conversion-map.md)。

ドキュメントの正本は英語です。ユーザー向けのページには日本語版があり、英語版と相互にリンクしています。

## ソースからのビルド

前提条件とビルドコマンドは [BUILD.md](BUILD.md) を参照してください。

Rust と Cargo をインストールしたあと、Rust コアと CLI を次のようにビルドします。

```sh
cargo fetch --locked --manifest-path rust/Cargo.toml
./tools/build-rust-core.sh --locked
printf '%s\n' '{"op":"new"}' | ./out/rust/debug/soul2d-core
```

CLI は JSON で応答します。stdio MCP を使うには次を実行します。

```sh
./out/rust/debug/soul2d-core --mcp
```

実験的な Rust GUI は次のとおりです。

```sh
./tools/build-rust-core.sh --locked --features gui
./out/rust/debug/soul2d-editor
```

GUI にはグラフィカルなデスクトップ環境と OpenGL が必要です。

## 継続的インテグレーション

CI は `main` 宛てのプルリクエスト、`main` への push、手動実行で動きます。

- [Rust development build](.github/workflows/rust-core.yml) は Linux でコア、CLI、GUI、テストターゲットを
  コンパイルし、ヘッドレスの X11 と Wayland でエディタを起動します。
- [Packaging](.github/workflows/packaging.yml) は、パッケージの元になるファイルが変わったときに
  Flatpak バンドルと Windows MSI をビルドし、簡単な動作確認をします。

CI はテストスイートを実行せず、署名もリリースの公開もしません。
ビルドが通っても、実行時の動作や形式の互換性は保証されません。

## 貢献と不具合報告

Soul2D の問題は[このリポジトリの Issue トラッカー](https://github.com/geekjapan/Soul2D/issues)に報告してください。
コミットハッシュ、プラットフォーム、ビルドコマンド、再現手順を含めてください。
ログやスクリーンショットから認証情報、個人のパス、モデルデータを取り除いてください。
再配布の許可がない第三者のモデルは添付しないでください。

変更は目的を絞り、形式を編集するときは未知のソースデータを保持し、変更した動作を確認してください。
手動の確認手順と任意の自動テストは [BUILD.md](BUILD.md) に記載しています。

## ライセンスと謝辞

プロジェクトのコードは [BSD-2-Clause](LICENSE) でライセンスされています。
元の Inochi2D と nijigenerate の著作権表示と[貢献者のクレジット](CONTRIBUTORS.md)を保持しています。

プロジェクトのアートワークは自作で、`tools/generate-project-artwork.py` で生成しています
（[res/ARTWORK.md](res/ARTWORK.md) を参照）。引き継いだアートワークは BSD-2-Clause のプロジェクト条件の下で
置き換えました。同梱のフォント、取り込んだコード、外部の依存関係には、それぞれの表示と条件があります。
[THIRD_PARTY.md](THIRD_PARTY.md) を参照してください。
BSD ライセンスは、第三者のモデル、ブランド、商標に対する権利を付与しません。Live2D と Cubism の名称は
形式と互換性の対象を示すためのものです。Soul2D は独立したプロジェクトであり、Live2D Inc. とは関係ありません。

現在の第三者ライセンス表示と公開上の制約は [THIRD_PARTY.md](THIRD_PARTY.md) を参照してください。
