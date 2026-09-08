(dispatch => {
  const freeze = Object.freeze.bind(Object), create = Object.create.bind(Object);
  const methods = /*METHODS*/;
  const platform = create(null);
  for (const method of methods) {
    const [namespace, name] = method.name.split('.');
    platform[namespace] ??= create(null);
    platform[namespace][name] = (...values) => {
      const args = create(null);
      for (let i = 0; i < method.parameters.length; i++) {
        const parameter = method.parameters[i];
        if (values[i] !== undefined) args[parameter.name] = values[i];
        else if (parameter.optional) args[parameter.name] = {};
      }
      return dispatch(method.name, args);
    };
  }
  for (const namespace of Object.values(platform)) freeze(namespace);
  return freeze(platform);
})
