[English](../en/01-model-structuring.md) | 日本語

# モデル構成チュートリアル（はじめての Node / Part 設計）

キャラクターモデルを組むときに「どこをどう分けて、どんな順番で親子を作るか」をまとめたチュートリアルです。

## 読み方

- 最初に全体像をつかみたい  
  → [01. 全体像と目標ツリー](./01-model-structuring/01-overview.md)
- 操作に迷ったときの判断基準が欲しい  
  → [02. 判断ガイド](./01-model-structuring/02-decision-guide.md)
- 作業に入る前の前提だけ確認したい  
  → [03. 準備](./01-model-structuring/03-preparation.md)
- 手順を順番に追いたい  
  → [04. ステップ 1：Root 直下に大ブロックを作る](./01-model-structuring/04-step-1-root-blocks.md)  
  → [05. ステップ 2：首・顔・目・口の親子関係を整える](./01-model-structuring/05-step-2-face-hierarchy.md)  
  → [06. ステップ 3：手足・後頭部の髪を付け根から先端に並べる](./01-model-structuring/06-step-3-chain-parts.md)  
  → [07. ステップ 4：Node / Composite / DynamicComposite を親として挿入する](./01-model-structuring/07-step-4-insert-helper-nodes.md)  
  → [08. ステップ 5：GridDeformer を必要な場所だけに入れる](./01-model-structuring/08-step-5-grid-deformer.md)  
  → [09. ステップ 6：前髪の Grid と胸パーツ、衣装の配置](./01-model-structuring/09-step-6-front-hair-chest-clothes.md)  
  → [10. ステップ 7：AutoMesh の設定と一括操作](./01-model-structuring/10-step-7-automesh.md)  
  → [11. ステップ 8：左右反転ペアを登録する](./01-model-structuring/11-step-8-symmetry-pairs.md)
- 最後の確認と完成イメージを見たい  
  → [12. チェックポイントと参考ツリー](./01-model-structuring/12-checklist-and-tree.md)

## 次の章

- モデル構成ができたあと、基本パラメータを作る  
  → [パラメータ定義チュートリアル](./02-parameter-definition.md)
