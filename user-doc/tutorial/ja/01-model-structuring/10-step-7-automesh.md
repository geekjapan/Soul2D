[English](../../en/01-model-structuring/10-step-7-automesh.md) | 日本語

# 10. ステップ 7：AutoMesh の設定と一括操作

ツリー構成と GridDeformer の配置ができたら、メッシュを張ります。  
ここでは、広い面を動かすためのグリッドと、各 Part の形に沿ったメッシュを分けて扱います。

## 基本ルール

`Part` には、基本的に `Optimum` アルゴリズムで AutoMesh を掛けます。Part を選び、ツールパネルの「Auto mesh」で Algorithm（Grid、Contour、Optimum、Skeleton）を選び、「Generate mesh」を押します。MCP では `automesh_generate` を使います。変形バインディング、ウェルド、深度データがすでにある Part には AutoMesh を掛けられないので、パラメータを追加する前にメッシュを張っておきます。  
AutoMesh の対象は Part だけです。`GridDeformer` には AutoMesh はなく、Edit Mesh の Grid Vertex Tool で格子を定義します（ステップ 5 参照）。

## 仕上げの流れ

1. 顔、胴体、前髪など、まとめて変形したい範囲に GridDeformer が入っていることを確認する。
2. GridDeformer の子に、変形させたい Part 群が入っていることを確認する。
3. 各 GridDeformer の格子を、Edit Mesh の Grid Vertex Tool で定義する。
4. 各 Part には Optimum を掛ける。

この順番にすると、広い面を動かすためのグリッドと、各パーツの形に沿ったメッシュを分けて管理できます。

## よくある失敗

- AutoMesh で GridDeformer の格子が作られると思い込む
- パーツの形に沿わせたい Part に Grid アルゴリズムを使ってしまう（Optimum を使う）
- 顔や胴体などの広い面に GridDeformer を入れ忘れる
- 格子状に動かしたい範囲と、パーツ形状に沿わせたい範囲を同じ設定で処理してしまう

## 複数パーツの一括操作

同じノード操作を似たパーツに繰り返したいときは、複数選択による一括操作が便利です。  
ノードの挿入、追加（選んだノードそれぞれに別々の新しいノードができます）、複数の子ノードを同じ親ノードに移す場合に使えます。AutoMesh は 1 つの Part ずつ実行します。Algorithm と Preset をそろえたまま、Part ごとに「Generate mesh」を押してください。

まとめて操作したあとは、意図した対象だけが変わっているかをツリー上で確認してください。

## 次に読む

- 左右反転ペアを登録する  
  → [11. ステップ 8：左右反転ペアを登録する](./11-step-8-symmetry-pairs.md)
