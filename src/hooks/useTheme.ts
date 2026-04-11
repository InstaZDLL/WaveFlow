import { createContext, useContext, type MouseEvent as ReactMouseEvent } from "react";

interface ThemeContextValue {
  isDark: boolean;
  toggleTheme: (event?: ReactMouseEvent) => void;
}

export const ThemeContext = createContext<ThemeContextValue | null>(null);

export function useTheme() {
  const context = useContext(ThemeContext);
  if (!context) throw new Error("useTheme must be used within ThemeProvider");
  return context;
}
