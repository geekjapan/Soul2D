[English](../../en/01-model-structuring/08-step-5-grid-deformer.md) | 日本語

# 08. ステップ 5：GridDeformer を必要な場所だけに入れる

GridDeformer は、顔や胴体のような広い面をまとめて変形するために使います。  
便利ですが、入れすぎると管理が難しくなるため、必要な場所に絞って配置します。

## 入れる場所の目安

基本的には、次の場所に GridDeformer を入れます。

- 顔
- 後頭部
- 横髪
- 胴体
- 前髪
- 胸部が別パーツの場合の胸まわり

顔や胴体のような広い面は、Part ひとつひとつにメッシュを張るだけでは扱いづらくなります。先に GridDeformer で変形用のまとまりを作っておくと、表情や体の向きを後から自然に調整しやすくなります。

## 挿入方法

すでに同じ位置にまとめ用の `Node` が入っていて、それをグリッドにしたい場合は、その `Node` を `GridDeformer` に変換します（Convert To...）。まとめ用の `Node` がない場合は、変形させたい Part を選択し、その親として GridDeformer を挿入します（Insert → GridDeformer）。

AutoMesh が作るのは Part のメッシュだけなので、GridDeformer 自体には AutoMesh を掛けません。新しい GridDeformer は 1 マスだけの状態で始まるので、格子は自分で整えます。Edit Mesh の Grid Vertex Tool（点をドラッグし、ダブルクリックで線を追加・削除）を使うか、MCP なら `deformer_define_grid` を使います。広い面を動かす予定がある場合は、Part に AutoMesh を掛ける前に、その外側に GridDeformer を用意しておきます。

## 格子数とマージン

格子数は、顔や後頭部では 8×8 程度、胴体では 10×14 程度を目安にします。キャラクターの縦横比やディテールに合わせて調整してください。Grid Vertex Tool の「Grid cuts」は新しく描く格子の軸ごとの線の数なので、8 マスなら 9 を指定します。

格子は、覆う Part より全体サイズの約 10% 前後大きくするのを目安にします。余白があると、変形時にエッジが自然に動きやすくなります。格子の外に出た頂点は格子の端に張り付きます。MCP では、`deformer_validate` が格子からはみ出した子を一覧し、`deformer_extend_grid` が足りない線を追加します。

## 目・口の DynamicComposite とグリッド

目や口など、顔の差分管理に `DynamicComposite` を使う場合も、グリッドにしておくと変形の影響が見えやすくなります。まずは 6×6 から 8×8 程度を目安にして、必要に応じて増減します。

## 操作例

1. 顔の Part を選択する。
2. ノードツリーで右クリックし、Suffix 欄に `::G` と入力して Insert → GridDeformer を選び、`Face::G` という名前の親を作る。
3. `Face::G` の子に `Face` Part が残っていることを確認する。
4. 後頭部、横髪、胴体、前髪、胸部にも、必要に応じて GridDeformer を挿入する。

## 次に読む

- 前髪、胸パーツ、衣装の配置を整える  
  → [09. ステップ 6：前髪の Grid と胸パーツ、衣装の配置](./09-step-6-front-hair-chest-clothes.md)
