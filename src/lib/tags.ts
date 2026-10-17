import type { Tag } from "../types";
import { folderAncestors, folderDescendants, folderTree } from "./folders";
export interface TagNode { tag: Tag; children: TagNode[]; depth: number }
// Folders and tags share only their structural fields, without duplicating
// cycle/orphan handling. Keep tag-specific display metadata on the original.
const structural = (tags: Tag[]) => tags.map((tag) => ({ ...tag, unreadCount: 0 }));
export const tagPath = (id: number, tags: Tag[]) => folderAncestors(id, structural(tags)).reverse().map((t) => t.name).join(" / ");
export const tagDescendants = (id: number, tags: Tag[]) => folderDescendants(id, structural(tags));
export function tagTree(tags: Tag[]): TagNode[] {
  const byId = new Map(tags.map((tag) => [tag.id, tag]));
  const map = (node: ReturnType<typeof folderTree>[number]): TagNode => ({ tag: byId.get(node.folder.id)!, depth: node.depth, children: node.children.map(map) });
  return folderTree(structural(tags)).map(map);
}
export function flatTags(tags: Tag[]): { tag: Tag; depth: number }[] {
  const result: { tag: Tag; depth: number }[] = [];
  const visit = (node: TagNode) => { result.push({ tag: node.tag, depth: node.depth }); node.children.forEach(visit); };
  tagTree(tags).forEach(visit);
  return result;
}
