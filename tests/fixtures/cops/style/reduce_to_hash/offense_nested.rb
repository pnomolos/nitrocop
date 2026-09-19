tables.each_with_object({}) { |table, h|
  h[table.node] = table.columns.each_with_object({}) { |column, i| i[column.name] = column.alias }
                                ^ Style/ReduceToHash: Use `to_h { ... }` instead of `each_with_object`.
}
