import { useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { ThemeContext } from "../hooks/useTheme";

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [isDark, setIsDark] = useState(false);

  const toggleTheme = (event?: ReactMouseEvent) => {
    const flipTheme = () => setIsDark((prev) => !prev);

    // Fallback: no View Transitions API support → instant swap
    if (typeof document === "undefined" || !document.startViewTransition) {
      flipTheme();
      return;
    }

    const transition = document.startViewTransition(flipTheme);

    // Radial reveal from the click point
    if (event) {
      const x = event.clientX;
      const y = event.clientY;
      const endRadius = Math.hypot(
        Math.max(x, window.innerWidth - x),
        Math.max(y, window.innerHeight - y)
      );

      transition.ready
        .then(() => {
          document.documentElement.animate(
            {
              clipPath: [
                `circle(0px at ${x}px ${y}px)`,
                `circle(${endRadius}px at ${x}px ${y}px)`,
              ],
            },
            {
              duration: 600,
              easing: "ease-in-out",
              pseudoElement: "::view-transition-new(root)",
            }
          );
        })
        .catch(() => {
          // Animation failed — the theme has still toggled via flipTheme()
        });
    }
  };

  return (
    <ThemeContext.Provider value={{ isDark, toggleTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}
