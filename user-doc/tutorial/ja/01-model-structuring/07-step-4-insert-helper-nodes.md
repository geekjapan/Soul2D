[English](../../en/01-model-structuring/07-step-4-insert-helper-nodes.md) | 日本語

# 07. ステップ 4：Node / Composite / DynamicComposite を親として挿入する

ここからは、補助用のノードを追加します。  
基本は、既存の子を別のツリーに引っ越すのではなく、動かしたい Part を選び、その親として必要なノードを挿入することです。

## 挿入と変換の使い分け

親を 1 段差し込みたい場合は、挿入を使います。たとえば、腕の付け根に回転用の Node を追加したい場合は、付け根の Part を選択して、その親として Node を挿入します。

すでにある Node の種類を変えたい場合は、変換を使います。たとえば、目や口の差分管理用に `Node` を `DynamicComposite` へ変換する場合です。`Node` から `DynamicComposite` に変換した直後は、`DynamicComposite` の Inspector にある「Resize automatically」（プロパティ名は `auto_resized`）を 2 回トグルして、表示範囲を確定させます。

## 用途の目安

- 目や口の差分管理には、`Composite` または `DynamicComposite` を使う
- 腕や脚の分節には、起点となる Part の親として `Node` を挿入する
- 目だけでなく口も `DynamicComposite` にすると、表情差分のルールを揃えやすい

## 操作例

1. 目の Part を 1 つ選択する。
2. ノードツリーで右クリックし、Insert → Composite または Insert → DynamicComposite を選ぶ。Insert は選んだノードそれぞれに別々の新しいノードを作るので、もう一方の目の Part は、あとで新しいノードの中へ移す（ツリー上でドラッグするか、「ノード」パネルの「Move destination」と「Move」を使う）。
3. Inspector の Name 欄で、`Eyes::Comp` など、用途が分かる名前を付ける。
4. 腕の付け根の Part を選択し、Insert → Node で `Arm R::Joint` のような補助ノードを作る。

## 次に読む

- 必要な場所に GridDeformer を入れる  
  → [08. ステップ 5：GridDeformer を必要な場所だけに入れる](./08-step-5-grid-deformer.md)
