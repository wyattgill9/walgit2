import { Suspense, lazy } from "react";
import { useParams } from "react-router-dom";

// react-markdown + remark-gfm (micromark, mdast, hast…) is ~100 kB gzipped;
// only pay for it when a README/markdown blob is actually rendered.
const Renderer = lazy(() => import("./MarkdownRenderer"));

/** `#12` → a link to issue or pull request 12 of `repo`, outside code spans and fences (GitHub's
 * autolink; `/issues/{n}` forwards a pull request to `/pull/{n}`). */
export function linkRefs(md: string, repo: string): string {
  return md
    .split(/(```[\s\S]*?```|`[^`\n]*`)/g)
    .map((part, i) => (i % 2 === 1 ? part : part.replace(/(^|[^\w&/#[])#(\d+)\b/g, (_, pre: string, n: string) => `${pre}[#${n}](/${repo}/issues/${n})`)))
    .join("");
}

export function Markdown({ source }: { source: string }) {
  const { owner, repo } = useParams();
  const text = owner && repo ? linkRefs(source, `${owner}/${repo}`) : source;
  return (
    <div className="markdown-body">
      <Suspense fallback={<pre className="code-block">{source}</pre>}>
        <Renderer source={text} />
      </Suspense>
    </div>
  );
}
