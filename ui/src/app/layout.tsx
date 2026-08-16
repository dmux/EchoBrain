import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";

// shadcn's theme reads `--font-sans` and `--font-geist-mono` (see globals.css),
// so the font loader has to publish exactly those names.
const geistSans = Geist({
  variable: "--font-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

import { ThemeProvider } from "@/components/theme-provider";

export const metadata: Metadata = {
  title: "EchoBrain",
  description: "Memória semântica por voz, sobre DynamoDB vector search.",
};

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html
      lang="pt-BR"
      className={`${geistSans.variable} ${geistMono.variable} h-full antialiased`}
      // The panel follows the system appearance. A toggle would be one more
      // thing to keep in sync with the native window chrome for no gain.
      suppressHydrationWarning
    >
      {/*
        `bg-transparent` is load-bearing on macOS: phase 3 turns this window into
        an NSPanel backed by NSVisualEffectView, and an opaque body would paint
        over the vibrancy. In the browser the gradient below stands in for it.
      */}
      <body className="flex min-h-full flex-col bg-transparent">
        <ThemeProvider
          attribute="class"
          defaultTheme="system"
          enableSystem
          disableTransitionOnChange
        >
          <div
            aria-hidden
            className="pointer-events-none fixed inset-0 -z-10 bg-background bg-[radial-gradient(60rem_40rem_at_50%_-10%,var(--color-sky-500)/8%,transparent),radial-gradient(40rem_30rem_at_90%_110%,var(--color-violet-500)/8%,transparent)]"
          />
          {children}
        </ThemeProvider>
      </body>
    </html>
  );
}
