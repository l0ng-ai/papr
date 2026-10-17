import { describe, expect, it } from "vitest";
import { folderAncestors, folderDescendants, folderPath, folderTree } from "./folders";
import type { Folder } from "../types";
const folders: Folder[] = [
  {id: 3, name: "Leaf", position: 0, parentId: 2},
  {id: 1, name: "Root", position: 1, parentId: null},
  {id: 2, name: "Child", position: 2, parentId: 1},
  {id: 4, name: "Other", position: 3, parentId: null},
];
describe("folder hierarchy", () => {
  it("builds nested directories even when children precede their parents", () => {
    const tree=folderTree(folders);
    expect(tree.map(n=>n.folder.id)).toEqual([1,4]);
    expect(tree[0].children[0].children[0].depth).toBe(2);
    expect(folderAncestors(3,folders).map(f=>f.id)).toEqual([3,2,1]);
    expect(folderPath(3,folders)).toBe("Root / Child / Leaf");
  });
  it("selects the complete subtree without unrelated folders", () => {
    expect([...folderDescendants(1,folders)]).toEqual([1,2,3]);
    expect([...folderDescendants(2,folders)]).toEqual([2,3]);
  });
  it("keeps orphans visible and bounds cyclic legacy data", () => {
    const broken=folders.map(f=>({...f,parentId:f.id===1?3:f.parentId}));
    expect(folderAncestors(3,broken)).toHaveLength(3);
    expect(folderTree(broken)).toHaveLength(2);
    expect(folderTree([{...folders[0],parentId:999}])[0].folder.id).toBe(3);
  });
});
