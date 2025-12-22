// @ts-nocheck
import { browser } from 'fumadocs-mdx/runtime/browser';
import type * as Config from '../source.config';

const create = browser<typeof Config, import("fumadocs-mdx/runtime/types").InternalTypeConfig & {
  DocData: {
  }
}>();
const browserCollections = {
  docs: create.doc("docs", {"index.mdx": () => import("../content/docs/index.mdx?collection=docs"), "architecture/compositing-buffer-design.md": () => import("../content/docs/architecture/compositing-buffer-design.md?collection=docs"), "architecture/dmx-database-and-fixture-loader.md": () => import("../content/docs/architecture/dmx-database-and-fixture-loader.md?collection=docs"), "architecture/harmony_analysis_and_spatial_attributes.md": () => import("../content/docs/architecture/harmony_analysis_and_spatial_attributes.md?collection=docs"), "architecture/node_graph_migration_analysis.md": () => import("../content/docs/architecture/node_graph_migration_analysis.md?collection=docs"), "architecture/post-cloud-local-db-plan.md": () => import("../content/docs/architecture/post-cloud-local-db-plan.md?collection=docs"), "architecture/qlcplus-capability-mapping.md": () => import("../content/docs/architecture/qlcplus-capability-mapping.md?collection=docs"), "architecture/selection-and-hierarchy-design.md": () => import("../content/docs/architecture/selection-and-hierarchy-design.md?collection=docs"), "architecture/terminology.md": () => import("../content/docs/architecture/terminology.md?collection=docs"), "architecture/unified-selection-and-capability-application.md": () => import("../content/docs/architecture/unified-selection-and-capability-application.md?collection=docs"), "concepts/semantic-layer.mdx": () => import("../content/docs/concepts/semantic-layer.mdx?collection=docs"), "getting-started/first-project.mdx": () => import("../content/docs/getting-started/first-project.mdx?collection=docs"), "getting-started/installation.mdx": () => import("../content/docs/getting-started/installation.mdx?collection=docs"), }),
};
export default browserCollections;