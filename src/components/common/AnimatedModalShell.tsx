import {
  AnimatePresence,
  motion,
  type HTMLMotionProps,
  type Transition,
} from "framer-motion";
import { forwardRef, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

interface AnimatedModalShellProps {
  isOpen: boolean;
  onBackdropClick?: () => void;
  children: ReactNode;
  backdropClassName?: string;
}

const BACKDROP_TRANSITION: Transition = { duration: 0.18, ease: "easeOut" };
const CONTENT_TRANSITION: Transition = {
  type: "spring",
  stiffness: 380,
  damping: 28,
  mass: 0.6,
};

export function AnimatedModalShell({
  isOpen,
  onBackdropClick,
  children,
  backdropClassName,
}: AnimatedModalShellProps) {
  // Where the press started. A `click` fires on the closest common ancestor
  // of mousedown and mouseup, so selecting text inside the dialog and
  // releasing past its edge — which is what dragging across a field to
  // select it does — targets the backdrop and closed the modal, discarding
  // whatever was being typed. Dismissal has to mean "pressed AND released on
  // the backdrop", which is also how a native dialog behaves.
  const pressedBackdrop = useRef(false);
  const content = (
    <AnimatePresence>
      {isOpen && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={BACKDROP_TRANSITION}
          onPointerDown={(e) => {
            pressedBackdrop.current = e.target === e.currentTarget;
          }}
          onClick={(e) => {
            // Both ends of the gesture on the backdrop itself, never on a
            // child that bubbled up here.
            if (!pressedBackdrop.current || e.target !== e.currentTarget) return;
            pressedBackdrop.current = false;
            onBackdropClick?.();
          }}
          className={
            backdropClassName ??
            "fixed inset-0 z-100 bg-black/80 backdrop-blur-md flex items-center justify-center p-4"
          }
        >
          {children}
        </motion.div>
      )}
    </AnimatePresence>
  );

  return createPortal(content, document.body);
}

type AnimatedModalContentProps = Omit<
  HTMLMotionProps<"div">,
  "initial" | "animate" | "exit" | "transition"
> & {
  children: ReactNode;
};

export const AnimatedModalContent = forwardRef<
  HTMLDivElement,
  AnimatedModalContentProps
>(function AnimatedModalContent(
  { children, className, onClick, ...rest },
  ref,
) {
  return (
    <motion.div
      ref={ref}
      initial={{ opacity: 0, scale: 0.95, y: 8 }}
      animate={{ opacity: 1, scale: 1, y: 0 }}
      exit={{ opacity: 0, scale: 0.97, y: 4 }}
      transition={CONTENT_TRANSITION}
      onClick={(e) => {
        e.stopPropagation();
        onClick?.(e);
      }}
      className={className}
      {...rest}
    >
      {children}
    </motion.div>
  );
});
