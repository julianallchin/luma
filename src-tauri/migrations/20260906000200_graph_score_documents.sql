-- A score's graphs and clips form one authored document. NULL identifies an
-- existing score that has not yet been manually migrated.
ALTER TABLE scores ADD COLUMN graph_document_json TEXT CHECK (
    graph_document_json IS NULL OR (
        json_valid(graph_document_json)
        AND json_type(graph_document_json) = 'object'
        AND COALESCE(json_type(graph_document_json, '$.version') = 'integer', 0)
        AND COALESCE(json_type(graph_document_json, '$.definitions') = 'object', 0)
        AND COALESCE(json_type(graph_document_json, '$.clips') = 'object', 0)
    )
);
