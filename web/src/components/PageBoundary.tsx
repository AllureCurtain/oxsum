import { Component, Suspense, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useLocation } from "react-router-dom";
import { ChunkLoadError } from "../lib/chunk-reload";
import { Banner, Button, PageState } from "./ui";

/**
 * The boundaries around a lazy page (issue #1092). `Suspense` shows the
 * shared empty state while the page's chunk loads. The error boundary shows
 * a reload prompt in the page slot when the chunk cannot load, so the shell
 * stays usable, and opening another page clears it.
 */
export function PageBoundary({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  return (
    <ChunkErrorBoundary path={pathname}>
      <Suspense fallback={<PageState />}>{children}</Suspense>
    </ChunkErrorBoundary>
  );
}

type BoundaryProps = { path: string; children: ReactNode };
type BoundaryState = { caught: boolean; error: unknown };

/**
 * Catches what a lazy page throws while rendering. A `ChunkLoadError`
 * renders the reload prompt. Anything else is rethrown, so it fails the way
 * it did before this boundary existed.
 */
class ChunkErrorBoundary extends Component<BoundaryProps, BoundaryState> {
  state: BoundaryState = { caught: false, error: undefined };

  static getDerivedStateFromError(error: unknown): BoundaryState {
    return { caught: true, error };
  }

  componentDidUpdate(prevProps: BoundaryProps, prevState: BoundaryState) {
    // Sibling routes share this instance. The update that caught the error is
    // usually the navigation that changed the path, so only a path change made
    // while the prompt was already showing clears it.
    if (prevState.caught && prevProps.path !== this.props.path) {
      this.setState({ caught: false, error: undefined });
    }
  }

  render() {
    if (!this.state.caught) return this.props.children;
    if (this.state.error instanceof ChunkLoadError) return <ReloadPrompt />;
    throw this.state.error;
  }
}

/**
 * The page slot when the page's chunk still cannot load and the tab will not
 * reload itself. Only the message is the alert, so a screen reader announces
 * it without the button's label.
 */
function ReloadPrompt() {
  const { t } = useTranslation();
  return (
    <Banner tone="error">
      <span role="alert">{t("common.pageLoadFailed")}</span>{" "}
      <Button tone="ghost" onClick={() => window.location.reload()}>
        {t("common.reload")}
      </Button>
    </Banner>
  );
}
