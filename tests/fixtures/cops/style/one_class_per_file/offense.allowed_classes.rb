# nitrocop-config: AllowedClasses: [ErrorA, ErrorB]
class Foo
end

class ErrorA
end

module ErrorB
end

class Bar
^^^^^^^^^ Style/OneClassPerFile: Do not define multiple classes/modules at the top level in a single file.
end

class ErrorA::Nested
^^^^^^^^^^^^^^^^^^^^ Style/OneClassPerFile: Do not define multiple classes/modules at the top level in a single file.
end
