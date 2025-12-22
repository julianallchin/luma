// @ts-nocheck
import * as __fd_glob_16 from "../content/docs/getting-started/installation.mdx?collection=docs"
import * as __fd_glob_15 from "../content/docs/getting-started/first-project.mdx?collection=docs"
import * as __fd_glob_14 from "../content/docs/concepts/semantic-layer.mdx?collection=docs"
import * as __fd_glob_13 from "../content/docs/architecture/unified-selection-and-capability-application.md?collection=docs"
import * as __fd_glob_12 from "../content/docs/architecture/terminology.md?collection=docs"
import * as __fd_glob_11 from "../content/docs/architecture/selection-and-hierarchy-design.md?collection=docs"
import * as __fd_glob_10 from "../content/docs/architecture/qlcplus-capability-mapping.md?collection=docs"
import * as __fd_glob_9 from "../content/docs/architecture/post-cloud-local-db-plan.md?collection=docs"
import * as __fd_glob_8 from "../content/docs/architecture/node_graph_migration_analysis.md?collection=docs"
import * as __fd_glob_7 from "../content/docs/architecture/harmony_analysis_and_spatial_attributes.md?collection=docs"
import * as __fd_glob_6 from "../content/docs/architecture/dmx-database-and-fixture-loader.md?collection=docs"
import * as __fd_glob_5 from "../content/docs/architecture/compositing-buffer-design.md?collection=docs"
import * as __fd_glob_4 from "../content/docs/index.mdx?collection=docs"
import { default as __fd_glob_3 } from "../content/docs/getting-started/meta.json?collection=docs"
import { default as __fd_glob_2 } from "../content/docs/concepts/meta.json?collection=docs"
import { default as __fd_glob_1 } from "../content/docs/architecture/meta.json?collection=docs"
import { default as __fd_glob_0 } from "../content/docs/meta.json?collection=docs"
import { server } from 'fumadocs-mdx/runtime/server';
import type * as Config from '../source.config';

const create = server<typeof Config, import("fumadocs-mdx/runtime/types").InternalTypeConfig & {
  DocData: {
  }
}>({"doc":{"passthroughs":["extractedReferences"]}});

export const docs = await create.docs("docs", "content/docs", {"meta.json": __fd_glob_0, "architecture/meta.json": __fd_glob_1, "concepts/meta.json": __fd_glob_2, "getting-started/meta.json": __fd_glob_3, }, {"index.mdx": __fd_glob_4, "architecture/compositing-buffer-design.md": __fd_glob_5, "architecture/dmx-database-and-fixture-loader.md": __fd_glob_6, "architecture/harmony_analysis_and_spatial_attributes.md": __fd_glob_7, "architecture/node_graph_migration_analysis.md": __fd_glob_8, "architecture/post-cloud-local-db-plan.md": __fd_glob_9, "architecture/qlcplus-capability-mapping.md": __fd_glob_10, "architecture/selection-and-hierarchy-design.md": __fd_glob_11, "architecture/terminology.md": __fd_glob_12, "architecture/unified-selection-and-capability-application.md": __fd_glob_13, "concepts/semantic-layer.mdx": __fd_glob_14, "getting-started/first-project.mdx": __fd_glob_15, "getting-started/installation.mdx": __fd_glob_16, });