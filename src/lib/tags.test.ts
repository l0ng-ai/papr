import { describe, expect, it } from "vitest";
import { flatTags, tagDescendants, tagPath, tagTree } from "./tags";
import type { Tag } from "../types";
const tags: Tag[] = [
  { id: 3, name: "Async", parentId: 2, position: 0, color: "blue", articleCount: 1 },
  { id: 1, name: "Development", parentId: null, position: 1, color: "green", articleCount: 1 },
  { id: 2, name: "Rust", parentId: 1, position: 2, color: "red", articleCount: 1 },
];
describe("tag hierarchy", () => {
  it("uses the same tree order and full paths in sidebar and tag picker", () => {
    expect(flatTags(tags).map(({tag,depth}) => [tag.id,depth])).toEqual([[1,0],[2,1],[3,2]]);
    expect(tagPath(3,tags)).toBe("Development / Rust / Async");
    expect([...tagDescendants(1,tags)]).toEqual([1,2,3]);
  });
  it("keeps orphaned and cyclic imported tags visible without recursion loops", () => {
    expect(tagTree([{...tags[0],parentId:999}])[0].tag.id).toBe(3);
    expect(flatTags(tags.map(t => t.id === 1 ? {...t,parentId:3} : t))).toHaveLength(3);
  });
});
