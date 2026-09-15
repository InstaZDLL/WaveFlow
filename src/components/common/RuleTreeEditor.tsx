import { useTranslation } from "react-i18next";
import { Plus, Trash2, FolderTree, Ban, Star } from "lucide-react";
import type {
  Predicate,
  PredicateKind,
  RuleNode,
} from "../../lib/tauri/smart_playlists";
import type { GenreRow } from "../../lib/tauri/browse";
import type { TrackTagKey } from "../../lib/tauri/trackTags";

/**
 * Recursive visual editor for the smart playlist rule tree.
 *
 * The component is structurally co-recursive with the data: every
 * `NodeView` renders the right widget for `node.type` and threads an
 * `onChange(next: RuleNode)` callback so the parent can splice the new
 * subtree back into its children. This keeps path arithmetic out of
 * the editor — each level only knows about its direct children.
 *
 * No drag-and-drop yet. Re-parenting is done via the "Wrap in group /
 * NOT" actions on each leaf, which is enough for the common case
 * (build a nested rule incrementally from the inside out).
 */

interface RuleTreeEditorProps {
  root: RuleNode;
  onChange: (next: RuleNode) => void;
  genres: GenreRow[];
  /** Tag keys the library actually holds (#588), with their counts. */
  tagKeys: TrackTagKey[];
}

export function RuleTreeEditor({
  root,
  onChange,
  genres,
  tagKeys,
}: RuleTreeEditorProps) {
  return (
    <NodeView
      node={root}
      onChange={onChange}
      onDelete={null} // root can't be deleted
      depth={0}
      genres={genres}
      tagKeys={tagKeys}
    />
  );
}

// =============================================================================
// NodeView — recursive
// =============================================================================

interface NodeViewProps {
  node: RuleNode;
  onChange: (next: RuleNode) => void;
  /** `null` for the root, otherwise removes this subtree from the
   *  parent's `children`. The parent decides what to splice in. */
  onDelete: (() => void) | null;
  depth: number;
  genres: GenreRow[];
  tagKeys: TrackTagKey[];
}

function NodeView(props: NodeViewProps) {
  switch (props.node.type) {
    case "all":
    case "any":
      return <GroupView {...props} node={props.node} />;
    case "not":
      return <NotView {...props} node={props.node} />;
    case "leaf":
      return <LeafView {...props} node={props.node} />;
  }
}

// =============================================================================
// Group (AND / OR)
// =============================================================================

function GroupView({
  node,
  onChange,
  onDelete,
  depth,
  genres,
  tagKeys,
}: NodeViewProps & { node: Extract<RuleNode, { type: "all" | "any" }> }) {
  const { t } = useTranslation();
  const isAnd = node.type === "all";

  const updateChild = (idx: number, next: RuleNode) => {
    const children = [...node.children];
    children[idx] = next;
    onChange({ ...node, children });
  };
  const deleteChild = (idx: number) => {
    const children = node.children.filter((_, i) => i !== idx);
    onChange({ ...node, children });
  };
  const addLeaf = () => {
    onChange({
      ...node,
      children: [
        ...node.children,
        {
          type: "leaf",
          predicate: { kind: "title_contains", value: "" },
        },
      ],
    });
  };
  const addGroup = (op: "all" | "any") => {
    onChange({
      ...node,
      children: [...node.children, { type: op, children: [] }],
    });
  };
  const addNot = () => {
    onChange({
      ...node,
      children: [
        ...node.children,
        {
          type: "not",
          child: {
            type: "leaf",
            predicate: { kind: "liked" },
          },
        },
      ],
    });
  };
  const toggleOp = () => {
    onChange({
      ...node,
      type: isAnd ? "any" : "all",
    });
  };

  return (
    <div
      className={`rounded-2xl border ${
        isAnd
          ? "border-emerald-500/30 bg-emerald-500/5"
          : "border-violet-500/30 bg-violet-500/5"
      } p-3 space-y-2`}
      style={{ marginLeft: depth === 0 ? 0 : undefined }}
    >
      {/* Header */}
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={toggleOp}
          className={`text-xs font-bold uppercase tracking-widest px-2.5 py-1 rounded-md transition-colors ${
            isAnd
              ? "bg-emerald-500 text-white hover:bg-emerald-600"
              : "bg-violet-500 text-white hover:bg-violet-600"
          }`}
          title={t("smartPlaylistEditor.tree.toggleOpHint")}
        >
          {isAnd
            ? t("smartPlaylistEditor.tree.and")
            : t("smartPlaylistEditor.tree.or")}
        </button>
        <span className="text-xs text-zinc-500 dark:text-zinc-400">
          {node.children.length === 0
            ? isAnd
              ? t("smartPlaylistEditor.tree.matchAllEmpty")
              : t("smartPlaylistEditor.tree.matchNoneEmpty")
            : t("smartPlaylistEditor.tree.childCount", {
                count: node.children.length,
              })}
        </span>
        <div className="flex-1" />
        {onDelete && (
          <button
            type="button"
            onClick={onDelete}
            className="p-1.5 rounded-md text-zinc-400 hover:text-red-500 hover:bg-red-500/10 transition-colors"
            aria-label={t("smartPlaylistEditor.tree.deleteNode")}
          >
            <Trash2 size={14} />
          </button>
        )}
      </div>

      {/* Children */}
      {node.children.length > 0 && (
        <div className="space-y-2 pl-3 border-l-2 border-zinc-200 dark:border-zinc-700">
          {node.children.map((child, idx) => (
            <NodeView
              key={idx}
              node={child}
              onChange={(next) => updateChild(idx, next)}
              onDelete={() => deleteChild(idx)}
              depth={depth + 1}
              genres={genres}
              tagKeys={tagKeys}
            />
          ))}
        </div>
      )}

      {/* Add bar */}
      <div className="flex items-center gap-1.5 flex-wrap pt-1">
        <button
          type="button"
          onClick={addLeaf}
          className="inline-flex items-center gap-1 px-2.5 py-1 text-xs rounded-md bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
        >
          <Plus size={12} />
          {t("smartPlaylistEditor.tree.addCondition")}
        </button>
        <button
          type="button"
          onClick={() => addGroup("any")}
          className="inline-flex items-center gap-1 px-2.5 py-1 text-xs rounded-md bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
        >
          <FolderTree size={12} />
          {t("smartPlaylistEditor.tree.addGroupOr")}
        </button>
        <button
          type="button"
          onClick={() => addGroup("all")}
          className="inline-flex items-center gap-1 px-2.5 py-1 text-xs rounded-md bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
        >
          <FolderTree size={12} />
          {t("smartPlaylistEditor.tree.addGroupAnd")}
        </button>
        <button
          type="button"
          onClick={addNot}
          className="inline-flex items-center gap-1 px-2.5 py-1 text-xs rounded-md bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
        >
          <Ban size={12} />
          {t("smartPlaylistEditor.tree.addNot")}
        </button>
      </div>
    </div>
  );
}

// =============================================================================
// NOT
// =============================================================================

function NotView({
  node,
  onChange,
  onDelete,
  depth,
  genres,
  tagKeys,
}: NodeViewProps & { node: Extract<RuleNode, { type: "not" }> }) {
  const { t } = useTranslation();
  return (
    <div className="rounded-2xl border border-red-500/30 bg-red-500/5 p-3 space-y-2">
      <div className="flex items-center gap-2">
        <span className="text-xs font-bold uppercase tracking-widest px-2.5 py-1 rounded-md bg-red-500 text-white">
          {t("smartPlaylistEditor.tree.not")}
        </span>
        <span className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("smartPlaylistEditor.tree.notHint")}
        </span>
        <div className="flex-1" />
        {onDelete && (
          <button
            type="button"
            onClick={onDelete}
            className="p-1.5 rounded-md text-zinc-400 hover:text-red-500 hover:bg-red-500/10 transition-colors"
            aria-label={t("smartPlaylistEditor.tree.deleteNode")}
          >
            <Trash2 size={14} />
          </button>
        )}
      </div>
      <div className="pl-3 border-l-2 border-zinc-200 dark:border-zinc-700">
        <NodeView
          node={node.child}
          onChange={(next) => onChange({ ...node, child: next })}
          onDelete={null} // NOT can't have an empty child
          depth={depth + 1}
          genres={genres}
          tagKeys={tagKeys}
        />
      </div>
    </div>
  );
}

// =============================================================================
// Leaf — single predicate
// =============================================================================

interface PredicateOption {
  kind: PredicateKind;
  key: string; // i18n key suffix
}

/**
 * The predicates, in named groups.
 *
 * Twenty-four entries in one flat list is a list nobody reads to the
 * end; grouped, the one being looked for is in the section its name
 * suggests. The groups are rendered as `<optgroup>`, so the select
 * still behaves like a select — no custom popup to keep accessible.
 */
const PREDICATE_GROUPS: { key: string; options: PredicateOption[] }[] = [
  {
    key: "text",
    options: [
      { kind: "title_contains", key: "titleContains" },
      { kind: "artist_contains", key: "artistContains" },
      { kind: "album_contains", key: "albumContains" },
      { kind: "path_contains", key: "pathContains" },
    ],
  },
  {
    key: "library",
    options: [
      { kind: "genre_is", key: "genreIs" },
      { kind: "year_min", key: "yearMin" },
      { kind: "year_max", key: "yearMax" },
      { kind: "disc_number_is", key: "discNumberIs" },
      { kind: "liked", key: "liked" },
      { kind: "rating_min", key: "ratingMin" },
      { kind: "added_in_last_days", key: "addedInLastDays" },
    ],
  },
  {
    key: "audio",
    options: [
      { kind: "format", key: "format" },
      { kind: "hi_res", key: "hiRes" },
      { kind: "sample_rate_min", key: "sampleRateMin" },
      { kind: "bit_depth_min", key: "bitDepthMin" },
      { kind: "bpm_min", key: "bpmMin" },
      { kind: "bpm_max", key: "bpmMax" },
      { kind: "duration_min_ms", key: "durationMinMs" },
      { kind: "duration_max_ms", key: "durationMaxMs" },
    ],
  },
  {
    key: "history",
    options: [
      { kind: "play_count_min", key: "playCountMin" },
      { kind: "play_count_max", key: "playCountMax" },
      { kind: "played_in_last_days", key: "playedInLastDays" },
    ],
  },
  {
    key: "tags",
    options: [
      { kind: "tag_present", key: "tagPresent" },
      { kind: "tag_contains", key: "tagContains" },
    ],
  },
];

/**
 * The i18n suffix of a predicate kind, from the catalogue above.
 *
 * The catalogue is the only list of the pairing, so looking the suffix
 * up in it is what keeps a label from going stale when a predicate is
 * renamed — a second mapping would be a second thing to forget.
 */
function predicateLabelKey(kind: PredicateKind): string {
  for (const group of PREDICATE_GROUPS) {
    const found = group.options.find((o) => o.kind === kind);
    if (found) return found.key;
  }
  return kind;
}

/** Sample rates worth offering — the ones files are actually made at. */
const SAMPLE_RATE_OPTIONS = [44100, 48000, 88200, 96000, 176400, 192000];
const BIT_DEPTH_OPTIONS = [16, 24, 32];

const FORMAT_OPTIONS = [
  "flac",
  "mp3",
  "aac",
  "ogg",
  "opus",
  "wav",
  "dsf",
  "dff",
];

function defaultPredicateFor(
  kind: PredicateKind,
  tagKeys: TrackTagKey[],
): Predicate {
  switch (kind) {
    case "title_contains":
    case "artist_contains":
    case "album_contains":
      return { kind, value: "" };
    case "genre_is":
      return { kind, value: 0 };
    case "year_min":
      return { kind, value: 2000 };
    case "year_max":
      return { kind, value: 2026 };
    case "bpm_min":
      return { kind, value: 60 };
    case "bpm_max":
      return { kind, value: 180 };
    case "duration_min_ms":
      return { kind, value: 60_000 };
    case "duration_max_ms":
      return { kind, value: 600_000 };
    case "format":
      return { kind, value: "flac" };
    case "hi_res":
    case "liked":
      return { kind };
    case "rating_min":
      return { kind, value: 153 }; // 3 stars (3 * 255 / 5)
    case "play_count_min":
      return { kind, value: 10 };
    case "play_count_max":
      // Zero, because "never played" is the reason this predicate
      // exists — no other rule can say it.
      return { kind, value: 0 };
    case "played_in_last_days":
    case "added_in_last_days":
      return { kind, value: 30 };
    case "sample_rate_min":
      return { kind, value: 88200 };
    case "bit_depth_min":
      return { kind, value: 24 };
    case "disc_number_is":
      return { kind, value: 1 };
    case "path_contains":
      return { kind, value: "" };
    case "tag_present":
      // The most-carried key, which is the one already at the top of
      // the picker: a rule opening on a tag three files have would read
      // as broken before the user has touched anything.
      return { kind, key: tagKeys[0]?.key ?? "" };
    case "tag_contains":
      return { kind, key: tagKeys[0]?.key ?? "", value: "" };
  }
}

function LeafView({
  node,
  onChange,
  onDelete,
  genres,
  tagKeys,
}: NodeViewProps & { node: Extract<RuleNode, { type: "leaf" }> }) {
  const { t } = useTranslation();
  const pred = node.predicate;

  const setKind = (kind: PredicateKind) => {
    onChange({ ...node, predicate: defaultPredicateFor(kind, tagKeys) });
  };
  const updateValue = (next: Predicate) => {
    onChange({ ...node, predicate: next });
  };

  return (
    <div className="rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 p-2.5">
      <div className="flex items-center gap-2 flex-wrap">
        <select
          value={pred.kind}
          onChange={(e) => setKind(e.target.value as PredicateKind)}
          className="text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-900 px-2 py-1.5 focus:outline-none focus:ring-2 focus:ring-violet-500"
        >
          {PREDICATE_GROUPS.map((group) => (
            <optgroup
              key={group.key}
              label={t(`smartPlaylistEditor.predicateGroups.${group.key}`)}
            >
              {group.options.map((p) => (
                <option key={p.kind} value={p.kind}>
                  {t(`smartPlaylistEditor.predicates.${p.key}`)}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
        <PredicateValue
          predicate={pred}
          onChange={updateValue}
          genres={genres}
          tagKeys={tagKeys}
        />
        <div className="flex-1" />
        {onDelete && (
          <button
            type="button"
            onClick={onDelete}
            className="p-1.5 rounded-md text-zinc-400 hover:text-red-500 hover:bg-red-500/10 transition-colors"
            aria-label={t("smartPlaylistEditor.tree.deleteNode")}
          >
            <Trash2 size={14} />
          </button>
        )}
      </div>
    </div>
  );
}

// =============================================================================
// Per-predicate value widgets
// =============================================================================

function PredicateValue({
  predicate,
  onChange,
  genres,
  tagKeys,
}: {
  predicate: Predicate;
  onChange: (p: Predicate) => void;
  genres: GenreRow[];
  tagKeys: TrackTagKey[];
}) {
  const { t, i18n } = useTranslation();
  // The reader's decimal separator, not a dot: `toFixed` would write
  // 88.2 into an editor where every other number is localized.
  const kHz = new Intl.NumberFormat(i18n.resolvedLanguage ?? i18n.language, {
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
  });
  // Every value widget names itself: the predicate select next to it
  // carries the meaning visually, but a screen reader reaching the
  // second control of the pair would announce only its value.
  const label = t(
    `smartPlaylistEditor.predicates.${predicateLabelKey(predicate.kind)}`,
  );
  const inputCls =
    "text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-900 px-2 py-1.5 focus:outline-none focus:ring-2 focus:ring-violet-500";

  switch (predicate.kind) {
    case "title_contains":
    case "artist_contains":
    case "album_contains":
      return (
        <input
          type="text"
          value={predicate.value}
          onChange={(e) => onChange({ ...predicate, value: e.target.value })}
          placeholder={t("smartPlaylistEditor.tree.contains")}
          className={`${inputCls} flex-1 min-w-0`}
        />
      );
    case "path_contains":
      return (
        <input
          type="text"
          value={predicate.value}
          onChange={(e) => onChange({ ...predicate, value: e.target.value })}
          placeholder={t("smartPlaylistEditor.tree.pathPlaceholder")}
          aria-label={label}
          className={`${inputCls} flex-1 min-w-0`}
        />
      );
    case "genre_is":
      return (
        <select
          value={predicate.value}
          onChange={(e) =>
            onChange({ ...predicate, value: Number(e.target.value) })
          }
          className={inputCls}
        >
          <option value={0}>{t("smartPlaylistEditor.tree.pickGenre")}</option>
          {genres.map((g) => (
            <option key={g.id} value={g.id}>
              {g.name}
            </option>
          ))}
        </select>
      );
    case "year_min":
    case "year_max":
    case "duration_min_ms":
    case "duration_max_ms":
    case "play_count_min":
    case "play_count_max":
    case "disc_number_is":
      return (
        <input
          type="number"
          value={predicate.value}
          onChange={(e) =>
            onChange({ ...predicate, value: parseInt(e.target.value, 10) || 0 })
          }
          className={`${inputCls} w-28`}
        />
      );
    case "bpm_min":
    case "bpm_max":
      return (
        <input
          type="number"
          step="1"
          value={predicate.value}
          onChange={(e) =>
            onChange({ ...predicate, value: Number(e.target.value) || 0 })
          }
          className={`${inputCls} w-24`}
        />
      );
    case "format":
      return (
        <select
          value={predicate.value}
          onChange={(e) => onChange({ ...predicate, value: e.target.value })}
          className={inputCls}
        >
          {FORMAT_OPTIONS.map((f) => (
            <option key={f} value={f}>
              {f.toUpperCase()}
            </option>
          ))}
        </select>
      );
    // A window is a number plus its unit, and the unit is the half that
    // makes it readable: "30" on its own says nothing, and the field is
    // narrow enough that the suffix has to sit outside it.
    case "played_in_last_days":
    case "added_in_last_days":
      return (
        <span className="inline-flex items-center gap-1.5">
          <input
            type="number"
            min={1}
            value={predicate.value}
            onChange={(e) =>
              onChange({
                ...predicate,
                value: parseInt(e.target.value, 10) || 0,
              })
            }
            aria-label={label}
            className={`${inputCls} w-20`}
          />
          <span className="text-xs text-zinc-500 dark:text-zinc-400">
            {t("smartPlaylistEditor.tree.days", { count: predicate.value })}
          </span>
        </span>
      );
    case "sample_rate_min":
      return (
        <select
          value={predicate.value}
          onChange={(e) =>
            onChange({ ...predicate, value: Number(e.target.value) })
          }
          aria-label={label}
          className={inputCls}
        >
          {/* A stored rule can name a rate this list does not offer —
              a hand-edited rule set, or a list that changes under a
              playlist. A controlled select with no matching option
              renders blank and rewrites the rule on the next change,
              so the value is kept as an option of its own. */}
          {!SAMPLE_RATE_OPTIONS.includes(predicate.value) && (
            <option value={predicate.value}>
              {`${kHz.format(predicate.value / 1000)} kHz`}
            </option>
          )}
          {SAMPLE_RATE_OPTIONS.map((hz) => (
            <option key={hz} value={hz}>
              {`${kHz.format(hz / 1000)} kHz`}
            </option>
          ))}
        </select>
      );
    case "bit_depth_min":
      return (
        <select
          value={predicate.value}
          onChange={(e) =>
            onChange({ ...predicate, value: Number(e.target.value) })
          }
          aria-label={label}
          className={inputCls}
        >
          {!BIT_DEPTH_OPTIONS.includes(predicate.value) && (
            <option value={predicate.value}>
              {t("smartPlaylistEditor.tree.bits", { value: predicate.value })}
            </option>
          )}
          {BIT_DEPTH_OPTIONS.map((bits) => (
            <option key={bits} value={bits}>
              {t("smartPlaylistEditor.tree.bits", { value: bits })}
            </option>
          ))}
        </select>
      );
    case "tag_present":
      return (
        <TagKeyPicker
          value={predicate.key}
          tagKeys={tagKeys}
          onChange={(key) => onChange({ ...predicate, key })}
          className={inputCls}
        />
      );
    case "tag_contains":
      return (
        <>
          <TagKeyPicker
            value={predicate.key}
            tagKeys={tagKeys}
            onChange={(key) => onChange({ ...predicate, key })}
            className={inputCls}
          />
          <input
            type="text"
            value={predicate.value}
            onChange={(e) => onChange({ ...predicate, value: e.target.value })}
            placeholder={t("smartPlaylistEditor.tree.contains")}
            aria-label={label}
            className={`${inputCls} flex-1 min-w-0`}
          />
        </>
      );
    case "hi_res":
    case "liked":
      return null; // unit predicate — no value
    case "rating_min":
      // Convert 0-255 POPM to 1-5 stars (rounded).
      return (
        <StarPicker
          stars={Math.max(1, Math.round((predicate.value / 255) * 5))}
          onChange={(s) =>
            onChange({ ...predicate, value: Math.round((s / 5) * 255) })
          }
        />
      );
  }
}

/**
 * The tag keys the library holds, with how many tracks carry each.
 *
 * The count is the whole reason this is a picker and not a text field:
 * it separates the tag on every track from the one on three, and it
 * proves the key exists — a typed key that matches nothing is a rule
 * that silently returns no tracks with nothing on screen to say why.
 *
 * A key stored in an existing rule but absent from the library (the
 * files were removed, or the rule came from another machine) is kept as
 * an extra option rather than dropped: opening a playlist must not
 * rewrite its rules.
 */
function TagKeyPicker({
  value,
  tagKeys,
  onChange,
  className,
}: {
  value: string;
  tagKeys: TrackTagKey[];
  onChange: (key: string) => void;
  className: string;
}) {
  const { t } = useTranslation();
  const known = tagKeys.some(
    (k) => k.key.toLowerCase() === value.toLowerCase(),
  );
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={className}
      aria-label={t("smartPlaylistEditor.tree.pickTag")}
    >
      {(value === "" || !known) && (
        <option value={value}>
          {value === "" ? t("smartPlaylistEditor.tree.pickTag") : value}
        </option>
      )}
      {tagKeys.map((k) => (
        <option key={k.key} value={k.key}>
          {`${k.key} (${k.count})`}
        </option>
      ))}
    </select>
  );
}

function StarPicker({
  stars,
  onChange,
}: {
  stars: number;
  onChange: (s: number) => void;
}) {
  return (
    <div className="flex items-center gap-0.5">
      {[1, 2, 3, 4, 5].map((s) => (
        <button
          key={s}
          type="button"
          onClick={() => onChange(s)}
          className="p-1 rounded hover:bg-zinc-100 dark:hover:bg-zinc-700"
        >
          <Star
            size={14}
            className={
              s <= stars
                ? "fill-yellow-400 text-yellow-400"
                : "text-zinc-300 dark:text-zinc-600"
            }
          />
        </button>
      ))}
    </div>
  );
}
