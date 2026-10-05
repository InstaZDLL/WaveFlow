import { useCallback, useMemo, useState } from "react";

export interface MultiSelectState<T extends { id: number }> {
  selectedIds: Set<number>;
  anchorId: number | null;
  isSelected: (id: number) => boolean;
  toggleOne: (id: number) => void;
  selectRange: (id: number, items: T[]) => void;
  setSingle: (id: number) => void;
  clear: () => void;
  count: number;
}

export function useMultiSelect<
  T extends { id: number },
>(): MultiSelectState<T> {
  const [selectedIds, setSelectedIds] = useState<Set<number>>(() => new Set());
  const [anchorId, setAnchorId] = useState<number | null>(null);

  const isSelected = useCallback(
    (id: number) => selectedIds.has(id),
    [selectedIds],
  );

  const toggleOne = useCallback((id: number) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
    setAnchorId(id);
  }, []);

  const setSingle = useCallback((id: number) => {
    setSelectedIds(new Set([id]));
    setAnchorId(id);
  }, []);

  const selectRange = useCallback(
    (id: number, items: T[]) => {
      // With no anchor (nothing picked yet, or the selection was just
      // cleared, which single-click play does on every play) or one no
      // longer in the list, this click starts the range. Without it two
      // Shift+clicks selected two lone rows and never what lay between.
      if (anchorId == null || !items.some((x) => x.id === anchorId)) {
        setAnchorId(id);
      }
      setSelectedIds((prev) => {
        const anchor = anchorId ?? id;
        const fromIdx = items.findIndex((x) => x.id === anchor);
        const toIdx = items.findIndex((x) => x.id === id);
        if (toIdx === -1) return prev;
        const start = fromIdx === -1 ? toIdx : Math.min(fromIdx, toIdx);
        const end = fromIdx === -1 ? toIdx : Math.max(fromIdx, toIdx);
        const next = new Set(prev);
        for (let i = start; i <= end; i++) {
          next.add(items[i].id);
        }
        return next;
      });
    },
    [anchorId],
  );

  const clear = useCallback(() => {
    setSelectedIds(new Set());
    setAnchorId(null);
  }, []);

  return useMemo(
    () => ({
      selectedIds,
      anchorId,
      isSelected,
      toggleOne,
      selectRange,
      setSingle,
      clear,
      count: selectedIds.size,
    }),
    [
      selectedIds,
      anchorId,
      isSelected,
      toggleOne,
      selectRange,
      setSingle,
      clear,
    ],
  );
}
