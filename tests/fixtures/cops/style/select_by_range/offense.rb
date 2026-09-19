array.select { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.select { |x| (1..10).cover?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.select { |x| (1...10).include?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.select { |x| !x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `select` with a range check.
array.filter { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `filter` with a range check.
array.filter { |x| (1..10).cover?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `filter` with a range check.
array.filter { |x| (1...10).include?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `filter` with a range check.
array.filter { |x| !x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `filter` with a range check.
array.find_all { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `find_all` with a range check.
array.find_all { |x| (1..10).cover?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `find_all` with a range check.
array.find_all { |x| (1...10).include?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `find_all` with a range check.
array.find_all { |x| !x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `find_all` with a range check.
array.reject { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `reject` with a range check.
array.reject { |x| (1..10).cover?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `reject` with a range check.
array.reject { |x| !x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `reject` with a range check.
array.reject { |x| !(1..10).include?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `reject` with a range check.
array.find { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep(...).first` to `find` with a range check.
array.find { |x| (1..10).cover?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep(...).first` to `find` with a range check.
array.find { |x| !x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v(...).first` to `find` with a range check.
array.detect { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep(...).first` to `detect` with a range check.
array.detect { |x| (1..10).cover?(x) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep(...).first` to `detect` with a range check.
array.detect { |x| !x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v(...).first` to `detect` with a range check.
array.select { _1.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.select { (1..10).cover?(_1) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.reject { !_1.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `reject` with a range check.
array.select { it.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.select { (1..10).cover?(it) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.find { !it.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v(...).first` to `find` with a range check.
array&.select { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
select { |x| x.between?(1, 10) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
array.select { |x| !(x.between?(1, 10)) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `select` with a range check.
array.select { |x| !((1..10).cover?(x)) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep_v` to `select` with a range check.
array.select { |x| x.between?(min, max) }
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.

array.select do |x|
^^^^^^^^^^^^^^^^^^ Style/SelectByRange: Prefer `grep` to `select` with a range check.
  x.between?(1, 10)
end
