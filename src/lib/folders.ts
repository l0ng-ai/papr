import type { Folder } from "../types";

export interface FolderNode { folder: Folder; children: FolderNode[]; depth: number }

export function folderAncestors(id: number, folders: Folder[]): Folder[] {
  const byId = new Map(folders.map((folder) => [folder.id, folder]));
  const result: Folder[] = [];
  const seen = new Set<number>();
  let current = byId.get(id);
  while (current && !seen.has(current.id)) {
    result.push(current);
    seen.add(current.id);
    current = current.parentId == null ? undefined : byId.get(current.parentId);
  }
  return result;
}

export function folderPath(id: number, folders: Folder[]): string {
  return folderAncestors(id, folders).reverse().map((folder) => folder.name).join(" / ");
}

export function folderDescendants(id: number, folders: Folder[]): Set<number> {
  const ids = new Set([id]);
  const pending = [id];
  while (pending.length) {
    const parent = pending.pop();
    for (const folder of folders) {
      if (folder.parentId === parent && !ids.has(folder.id)) {
        ids.add(folder.id);
        pending.push(folder.id);
      }
    }
  }
  return ids;
}

export function folderTree(folders: Folder[]): FolderNode[] {
  const seen = new Set<number>();
  const byId = new Map(folders.map((folder) => [folder.id, folder]));
  const visit = (folder: Folder, depth: number): FolderNode => {
    seen.add(folder.id);
    const children = folders.filter((f) => f.parentId === folder.id && !seen.has(f.id)).map((f) => visit(f, depth + 1));
    return { folder, children, depth };
  };
  const roots = folders.filter((folder) => folder.parentId == null || !byId.has(folder.parentId)).map((folder) => visit(folder, 0));
  // Keep folders visible even if an older/imported database has broken links.
  for (const folder of folders) if (!seen.has(folder.id)) roots.push(visit(folder, 0));
  return roots;
}
