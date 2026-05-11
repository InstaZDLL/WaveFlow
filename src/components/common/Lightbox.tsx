import { X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";

interface Props {
  src: string | null;
  alt?: string;
  isOpen: boolean;
  onClose: () => void;
}

export function Lightbox({ src, alt, isOpen, onClose }: Props) {
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  if (!isOpen || !src) return null;

  return (
    <div
      ref={dialogRef}
      className="fixed inset-0 bg-black/90 z-100 flex items-center justify-center animate-fade-in"
      onClick={onClose}
      role="dialog"
      aria-modal="true"
      aria-label={alt}
    >
      <button
        type="button"
        onClick={onClose}
        aria-label="Close"
        className="absolute top-4 right-4 p-2 rounded-full bg-white/10 hover:bg-white/20 text-white transition-colors"
      >
        <X size={20} />
      </button>
      <img
        src={src}
        alt={alt ?? ""}
        onClick={(e) => e.stopPropagation()}
        className="max-w-[90vw] max-h-[90vh] object-contain rounded-lg shadow-2xl"
      />
    </div>
  );
}
