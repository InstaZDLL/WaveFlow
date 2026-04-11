export function ProgressBar() {
  return (
    <div className="w-full flex items-center space-x-3 text-xs text-zinc-400">
      <span>0:00</span>
      <div className="flex-1 h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700">
        <div className="w-0 h-full bg-emerald-500 rounded-full relative group">
          <div className="absolute right-0 top-1/2 -translate-y-1/2 w-3 h-3 bg-white rounded-full shadow border border-zinc-200 hidden group-hover:block" />
        </div>
      </div>
      <span>0:00</span>
    </div>
  );
}
