positives = arr.select { |x| x > 0 }
negatives = arr.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_one
neg2 = arr.reject { |x| x > 0 }
pos2 = arr.select { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `reject` and `select` calls.
separator_two
pos3 = arr.filter { |x| x > 0 }
neg3 = arr.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `filter` and `reject` calls.
separator_three
pos4 = arr.find_all { |x| x > 0 }
neg4 = arr.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `find_all` and `reject` calls.
separator_four
pos5 = arr.select do |x|
  x > 0
end
neg5 = arr.reject do |x|
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
  x > 0
end
separator_five
pos6 = arr.select { _1 > 0 }
neg6 = arr.reject { _1 > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_six
pos7 = arr.select { it > 0 }
neg7 = arr.reject { it > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_seven
pos8 = foo.bar.select { |x| x > 0 }
neg8 = foo.bar.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_eight
pos9 = arr&.select { |x| x > 0 }
neg9 = arr&.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_nine
arr.select { |x| x > 0 }
arr.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_ten
@positives = arr.select { |x| x > 0 }
@negatives = arr.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_eleven
pos10 = arr.select { |x| x > 0 }
@neg10 = arr.reject { |x| x > 0 }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_twelve
pos11 = arr.select(&:positive?)
neg11 = arr.reject(&:positive?)
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_thirteen
neg12 = arr.reject(&:positive?)
pos12 = arr.select(&:positive?)
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `reject` and `select` calls.
separator_fourteen
pos13 = arr&.select(&:positive?)
neg13 = arr&.reject(&:positive?)
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_fifteen
pos14 = arr.select(&:positive?)
neg14 = arr.reject { |x| x.positive? }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_sixteen
pos15 = arr.select { |x| x.positive? }
neg15 = arr.reject(&:positive?)
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `reject` calls.
separator_seventeen
a1 = arr.select { |x| x.positive? }
b1 = arr.select { |x| !x.positive? }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `select` calls.
separator_eighteen
b2 = arr.select { |x| !x.positive? }
a2 = arr.select { |x| x.positive? }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `select` calls.
separator_nineteen
a3 = arr.reject { |x| x.positive? }
b3 = arr.reject { |x| !x.positive? }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `reject` and `reject` calls.
separator_twenty
a4 = arr.select { _1.positive? }
b4 = arr.select { !_1.positive? }
^ Style/PartitionInsteadOfDoubleSelect: Use `partition` instead of consecutive `select` and `select` calls.
