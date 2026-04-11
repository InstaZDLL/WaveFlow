import { useTranslation } from "react-i18next";
import { ArrowLeft, MessageSquare, Mail, ArrowRight } from "lucide-react";
import type { ViewId } from "../../types";

interface FeedbackViewProps {
  onNavigate: (view: ViewId) => void;
}

export function FeedbackView({ onNavigate }: FeedbackViewProps) {
  const { t } = useTranslation();

  return (
    <div className="max-w-4xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-4">
        <button
          type="button"
          onClick={() => onNavigate("home")}
          aria-label={t("common.back")}
          className="p-1 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
        >
          <ArrowLeft size={20} />
        </button>
        <div>
          <h1 className="text-3xl font-bold text-zinc-900 dark:text-white">
            {t("feedback.title")}
          </h1>
          <div className="w-8 h-1 bg-emerald-500 rounded-full mt-1" />
        </div>
      </div>

      {/* Hero Banner */}
      <div className="relative overflow-hidden rounded-3xl p-10 bg-linear-to-br from-emerald-900/80 to-zinc-900 text-white">
        <div className="flex items-start space-x-5">
          <div className="w-14 h-14 rounded-2xl bg-emerald-500/20 flex items-center justify-center shrink-0">
            <MessageSquare size={28} className="text-emerald-400" aria-hidden="true" />
          </div>
          <div>
            <h2 className="text-2xl font-bold mb-3">{t("feedback.hero.title")}</h2>
            <p className="text-zinc-400 leading-relaxed">
              {t("feedback.hero.description")}
            </p>
          </div>
        </div>
      </div>

      {/* Contact */}
      <section aria-labelledby="feedback-contact-heading">
        <h2
          id="feedback-contact-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("feedback.contact.section")}
        </h2>
        <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800/50 overflow-hidden">
          <button
            type="button"
            className="w-full flex items-center justify-between p-5 hover:bg-zinc-50 dark:hover:bg-zinc-800 transition-colors"
          >
            <div className="flex items-center space-x-4">
              <div className="w-12 h-12 rounded-xl bg-emerald-100 text-emerald-600 flex items-center justify-center dark:bg-emerald-950/60 dark:text-emerald-400">
                <Mail size={22} aria-hidden="true" />
              </div>
              <div className="text-left">
                <div className="text-sm font-semibold text-emerald-600 dark:text-emerald-400">
                  contact@waveflow.dev
                </div>
                <div className="text-xs text-zinc-400">
                  {t("feedback.contact.subtitle")}
                </div>
              </div>
            </div>
            <div className="w-10 h-10 rounded-full bg-emerald-100 text-emerald-600 flex items-center justify-center dark:bg-emerald-950/60 dark:text-emerald-400">
              <ArrowRight size={18} aria-hidden="true" />
            </div>
          </button>
        </div>
      </section>
    </div>
  );
}
