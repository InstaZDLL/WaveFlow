import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronDown,
  ChevronRight,
  Combine,
  Plus,
  Trash2,
  UserPlus,
  X,
} from "lucide-react";
import { usePageVirtualizer } from "../../../hooks/usePageVirtualizer";
import {
  addAgent,
  deleteLine,
  insertLine,
  lineBegin,
  lineText,
  mergeWithNext,
  patchAgent,
  patchLine,
  removeAgent,
  setLinesFromText,
  setLineText,
  setTranslation,
  splitUnits,
  unitsText,
} from "../../../lib/lyricsDocumentEdit";
import type {
  LyricsAgentKind,
  LyricsDocument,
  LyricsDocumentLine,
  LyricsMetadata,
} from "../../../lib/tauri/lyricsDocument";
import { agentColor, formatEditorTime, type EditDocument } from "./shared";

interface EditStepProps {
  doc: LyricsDocument;
  edit: EditDocument;
  selected: number;
  onSelect: (line: number) => void;
}

const FIELD =
  "rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 px-2.5 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-pink-500";
const LABEL =
  "text-[11px] font-medium uppercase tracking-wide text-zinc-500 dark:text-zinc-400";

/**
 * The Edit step: the song's details, its singers, and the lines with
 * what each one carries besides its text — the singer, the section, the
 * backing vocals, a translation and a romanization. The lines can also
 * be edited as one block of text, which is how lyrics get pasted in.
 */
export function EditStep({ doc, edit, selected, onSelect }: EditStepProps) {
  const { t } = useTranslation();
  const [asText, setAsText] = useState(doc.lines.length === 0);

  return (
    <div className="flex flex-col gap-5">
      <SongDetails doc={doc} edit={edit} />
      <Singers doc={doc} edit={edit} />
      <div className="flex items-center justify-between">
        <h3 className={LABEL}>
          {t("lyricsEditor.edit.lines", { n: doc.lines.length })}
        </h3>
        <div
          role="radiogroup"
          aria-label={t("lyricsEditor.edit.viewLabel")}
          className="flex rounded-full bg-zinc-100 dark:bg-zinc-800 p-0.5 text-xs"
        >
          {(["lines", "text"] as const).map((view) => {
            const checked = (view === "text") === asText;
            return (
              <button
                key={view}
                type="button"
                role="radio"
                aria-checked={checked}
                onClick={() => setAsText(view === "text")}
                className={`px-3 py-1 rounded-full font-medium transition-colors ${
                  checked
                    ? "bg-white dark:bg-zinc-700 text-zinc-900 dark:text-white shadow-sm"
                    : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-200"
                }`}
              >
                {t(`lyricsEditor.edit.view.${view}`)}
              </button>
            );
          })}
        </div>
      </div>
      {asText ? (
        <TextEditor doc={doc} edit={edit} onDone={() => setAsText(false)} />
      ) : (
        <LineList
          doc={doc}
          edit={edit}
          selected={selected}
          onSelect={onSelect}
        />
      )}
    </div>
  );
}

function SongDetails({
  doc,
  edit,
}: {
  doc: LyricsDocument;
  edit: EditDocument;
}) {
  const { t } = useTranslation();
  const m = doc.metadata;
  const field = (
    key: keyof Pick<LyricsMetadata, "title" | "artist" | "album" | "language">,
    width: string,
  ) => (
    <label className={`flex flex-col gap-1 ${width}`}>
      <span className={LABEL}>{t(`lyricsEditor.edit.meta.${key}`)}</span>
      <input
        className={FIELD}
        value={m[key] ?? ""}
        placeholder={
          key === "language"
            ? t("lyricsEditor.edit.meta.languagePlaceholder")
            : undefined
        }
        onChange={(e) => {
          const value = e.target.value;
          edit(
            (d) => ({
              ...d,
              metadata: { ...d.metadata, [key]: value || undefined },
            }),
            `meta-${key}`,
          );
        }}
      />
    </label>
  );
  return (
    <div className="flex flex-wrap gap-3">
      {field("title", "flex-1 min-w-40")}
      {field("artist", "flex-1 min-w-40")}
      {field("album", "flex-1 min-w-40")}
      {field("language", "w-28")}
    </div>
  );
}

function Singers({ doc, edit }: { doc: LyricsDocument; edit: EditDocument }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className={`${LABEL} mr-1`}>{t("lyricsEditor.edit.singers")}</span>
      {doc.agents.length === 0 && (
        <span className="text-sm text-zinc-500 dark:text-zinc-400">
          {t("lyricsEditor.edit.singersHint")}
        </span>
      )}
      {doc.agents.map((agent, k) => (
        <span
          key={agent.id}
          className="flex items-center gap-1 rounded-full border border-zinc-200 dark:border-zinc-700 py-0.5 pr-1 pl-2.5"
        >
          <span
            aria-hidden
            className="size-2 rounded-full"
            style={{ background: agentColor(k) }}
          />
          <input
            className="w-28 bg-transparent text-sm focus:outline-none"
            value={agent.name ?? ""}
            placeholder={agent.id}
            aria-label={t("lyricsEditor.edit.singerName")}
            onChange={(e) => {
              const name = e.target.value;
              edit(
                (d) => patchAgent(d, agent.id, { name: name || undefined }),
                `agent-${agent.id}`,
              );
            }}
          />
          <select
            className="bg-transparent text-xs text-zinc-500 dark:text-zinc-400 focus:outline-none"
            value={agent.kind === "group" ? "group" : "person"}
            aria-label={t("lyricsEditor.edit.singerKind")}
            onChange={(e) => {
              const kind = e.target.value as LyricsAgentKind;
              edit((d) => patchAgent(d, agent.id, { kind }));
            }}
          >
            <option value="person">{t("lyricsEditor.edit.kind.person")}</option>
            <option value="group">{t("lyricsEditor.edit.kind.group")}</option>
          </select>
          <button
            type="button"
            className="p-1 rounded-full hover:bg-zinc-100 dark:hover:bg-zinc-800 text-zinc-400 hover:text-red-500"
            title={t("lyricsEditor.edit.removeSinger")}
            aria-label={t("lyricsEditor.edit.removeSinger")}
            onClick={() => edit((d) => removeAgent(d, agent.id))}
          >
            <X size={12} />
          </button>
        </span>
      ))}
      <button
        type="button"
        onClick={() => edit(addAgent)}
        className="flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium border border-zinc-200 dark:border-zinc-700 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
      >
        <UserPlus size={13} />
        {t("lyricsEditor.edit.addSinger")}
      </button>
    </div>
  );
}

function TextEditor({
  doc,
  edit,
  onDone,
}: {
  doc: LyricsDocument;
  edit: EditDocument;
  onDone: () => void;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState(() => doc.lines.map(lineText).join("\n"));
  const timed = doc.lines.some((l) => lineBegin(l) !== undefined);
  return (
    <div className="flex flex-col gap-3">
      <textarea
        className={`${FIELD} min-h-72 resize-y leading-7 p-4`}
        value={text}
        spellCheck={false}
        placeholder={t("lyricsEditor.edit.textPlaceholder")}
        onChange={(e) => setText(e.target.value)}
      />
      <div className="flex flex-wrap items-center gap-3">
        <button
          type="button"
          onClick={() => {
            edit((d) => setLinesFromText(d, text));
            onDone();
          }}
          className="px-4 py-2 rounded-full bg-pink-500 hover:bg-pink-600 text-white text-sm font-medium transition-colors"
        >
          {t("lyricsEditor.edit.useText")}
        </button>
        {timed && (
          <span className="text-xs text-zinc-500 dark:text-zinc-400">
            {t("lyricsEditor.edit.useTextKeepsTiming")}
          </span>
        )}
      </div>
    </div>
  );
}

function LineList({
  doc,
  edit,
  selected,
  onSelect,
}: {
  doc: LyricsDocument;
  edit: EditDocument;
  selected: number;
  onSelect: (line: number) => void;
}) {
  const { t } = useTranslation();
  // The line whose text field takes the focus once it is drawn: the one
  // Enter just inserted. A virtualized row may not exist yet when the
  // edit lands, so the row focuses itself when it mounts.
  const [focusLine, setFocusLine] = useState<number | null>(null);
  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: doc.lines.length,
    estimateSize: () => 44,
    overscan: 10,
  });

  useEffect(() => {
    if (focusLine !== null) virtualizer.scrollToIndex(focusLine);
  }, [focusLine, virtualizer]);

  return (
    <div>
      <div
        ref={parentRef}
        className="relative"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {virtualizer.getVirtualItems().map((row) => (
          <div
            key={row.key}
            data-index={row.index}
            ref={virtualizer.measureElement}
            className="absolute inset-x-0 top-0 pb-1"
            style={{ transform: `translateY(${row.start - scrollMargin}px)` }}
          >
            <LineRow
              doc={doc}
              line={doc.lines[row.index]}
              i={row.index}
              edit={edit}
              selected={row.index === selected}
              focus={row.index === focusLine}
              onFocused={() => setFocusLine(null)}
              onSelect={onSelect}
              onInsertBelow={() => {
                edit((d) => insertLine(d, row.index + 1));
                onSelect(row.index + 1);
                setFocusLine(row.index + 1);
              }}
            />
          </div>
        ))}
      </div>
      <button
        type="button"
        onClick={() => {
          const at = doc.lines.length;
          edit((d) => insertLine(d, d.lines.length));
          onSelect(at);
          setFocusLine(at);
        }}
        className="mt-2 flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs font-medium border border-zinc-200 dark:border-zinc-700 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
      >
        <Plus size={13} />
        {t("lyricsEditor.edit.addLine")}
      </button>
    </div>
  );
}

function LineRow({
  doc,
  line,
  i,
  edit,
  selected,
  focus,
  onFocused,
  onSelect,
  onInsertBelow,
}: {
  doc: LyricsDocument;
  line: LyricsDocumentLine;
  i: number;
  edit: EditDocument;
  selected: boolean;
  focus: boolean;
  onFocused: () => void;
  onSelect: (line: number) => void;
  onInsertBelow: () => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  // What the field shows. The document keeps words, not the text typed:
  // a trailing space is no word, so drawing the field from the document
  // would eat the space the user just typed. The field is re-read only
  // when the document says something its draft does not — an undo, a
  // merge.
  const [draft, setDraft] = useState(() => lineText(line));
  const text = lineText(line);
  if (unitsText(splitUnits(draft)) !== text && draft !== text) {
    setDraft(text);
  }
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!focus) return;
    inputRef.current?.focus();
    onFocused();
  }, [focus, onFocused]);

  const agentIndex = doc.agents.findIndex((a) => a.id === line.agent);
  const translation = line.translations?.[0];
  const background = unitsText(line.background ?? []);

  return (
    <div
      className={`group rounded-lg px-2 py-1 transition-colors ${
        selected
          ? "bg-pink-50 dark:bg-pink-950/30 ring-1 ring-pink-200 dark:ring-pink-900"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
      }`}
      onFocus={() => onSelect(i)}
    >
      <div className="flex items-center gap-2">
        <button
          type="button"
          className="p-1 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400"
          aria-expanded={open}
          title={t("lyricsEditor.edit.details")}
          aria-label={t("lyricsEditor.edit.details")}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </button>
        <span className="w-7 text-right font-mono text-xs text-zinc-400 tabular-nums">
          {i + 1}
        </span>
        <span
          aria-hidden
          className="size-2 shrink-0 rounded-full"
          style={{
            background:
              agentIndex >= 0 ? agentColor(agentIndex) : "transparent",
          }}
        />
        <input
          ref={inputRef}
          className="min-w-0 flex-1 bg-transparent px-1 py-1 text-sm focus:outline-none"
          value={draft}
          placeholder={t("lyricsEditor.edit.linePlaceholder")}
          aria-label={t("lyricsEditor.edit.lineLabel", { n: i + 1 })}
          onChange={(e) => {
            const value = e.target.value;
            setDraft(value);
            edit((d) => setLineText(d, i, value), `line-${i}`);
          }}
          onKeyDown={(e) => {
            // Enter also picks a candidate while an input method composes
            // (Japanese, Chinese): that one is not for us.
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter") {
              e.preventDefault();
              onInsertBelow();
            }
          }}
        />
        {(line.translations?.length || line.romanization || background) && (
          <span className="text-[10px] font-medium uppercase text-pink-500 shrink-0">
            {[
              background && t("lyricsEditor.edit.badge.background"),
              line.translations?.length &&
                t("lyricsEditor.edit.badge.translation"),
              line.romanization && t("lyricsEditor.edit.badge.romanization"),
            ]
              .filter(Boolean)
              .join(" · ")}
          </span>
        )}
        <span className="font-mono text-xs text-zinc-400 tabular-nums shrink-0">
          {formatEditorTime(lineBegin(line))}
        </span>
        <div className="flex opacity-0 group-hover:opacity-100 focus-within:opacity-100">
          <button
            type="button"
            className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 disabled:opacity-30"
            title={t("lyricsEditor.edit.mergeNext")}
            aria-label={t("lyricsEditor.edit.mergeNext")}
            disabled={i + 1 >= doc.lines.length}
            onClick={() => edit((d) => mergeWithNext(d, i))}
          >
            <Combine size={13} />
          </button>
          <button
            type="button"
            className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400 hover:text-red-500"
            title={t("lyricsEditor.edit.removeLine")}
            aria-label={t("lyricsEditor.edit.removeLine")}
            onClick={() => edit((d) => deleteLine(d, i))}
          >
            <Trash2 size={13} />
          </button>
        </div>
      </div>
      {open && (
        <div className="mt-2 mb-1 grid grid-cols-[8rem_1fr] items-center gap-x-3 gap-y-2 pl-16 pr-2 text-sm">
          <span className={LABEL}>{t("lyricsEditor.edit.singer")}</span>
          <select
            className={FIELD}
            aria-label={t("lyricsEditor.edit.singer")}
            value={line.agent ?? ""}
            onChange={(e) => {
              const agent = e.target.value || undefined;
              edit((d) => patchLine(d, i, { agent }));
            }}
          >
            <option value="">{t("lyricsEditor.edit.lead")}</option>
            {doc.agents.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name ?? a.id}
              </option>
            ))}
          </select>
          <span className={LABEL}>{t("lyricsEditor.edit.part")}</span>
          <input
            className={FIELD}
            aria-label={t("lyricsEditor.edit.part")}
            value={line.part ?? ""}
            placeholder={t("lyricsEditor.edit.partPlaceholder")}
            onChange={(e) => {
              const part = e.target.value || undefined;
              edit((d) => patchLine(d, i, { part }), `part-${i}`);
            }}
          />
          <span className={LABEL}>{t("lyricsEditor.edit.background")}</span>
          <BackgroundField
            value={background}
            onChange={(value) => {
              const units = splitUnits(value);
              edit(
                (d) =>
                  patchLine(d, i, {
                    background: units.length ? units : undefined,
                  }),
                `bg-${i}`,
              );
            }}
          />
          <span className={LABEL}>{t("lyricsEditor.edit.translation")}</span>
          <div className="flex gap-2">
            <input
              className={`${FIELD} w-20`}
              value={translation?.lang ?? ""}
              placeholder={t("lyricsEditor.edit.translationLang")}
              aria-label={t("lyricsEditor.edit.translationLang")}
              onChange={(e) => {
                const lang = e.target.value || undefined;
                edit(
                  (d) => setTranslation(d, i, lang, translation?.text ?? ""),
                  `tl-${i}`,
                );
              }}
            />
            <input
              className={`${FIELD} flex-1`}
              value={translation?.text ?? ""}
              aria-label={t("lyricsEditor.edit.translation")}
              onChange={(e) => {
                const value = e.target.value;
                edit(
                  (d) => setTranslation(d, i, translation?.lang, value),
                  `tr-${i}`,
                );
              }}
            />
          </div>
          <span className={LABEL}>{t("lyricsEditor.edit.romanization")}</span>
          <input
            className={FIELD}
            aria-label={t("lyricsEditor.edit.romanization")}
            value={line.romanization ?? ""}
            onChange={(e) => {
              const romanization = e.target.value || undefined;
              edit((d) => patchLine(d, i, { romanization }), `ro-${i}`);
            }}
          />
        </div>
      )}
    </div>
  );
}

/** The backing vocals as text: held as words like the line, so the field
 *  keeps a draft for the same reason. */
function BackgroundField({
  value,
  onChange,
}: {
  value: string;
  onChange: (value: string) => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(value);
  if (unitsText(splitUnits(draft)) !== value && draft !== value) {
    setDraft(value);
  }
  return (
    <input
      className={FIELD}
      value={draft}
      aria-label={t("lyricsEditor.edit.background")}
      placeholder={t("lyricsEditor.edit.backgroundPlaceholder")}
      onChange={(e) => {
        setDraft(e.target.value);
        onChange(e.target.value);
      }}
    />
  );
}
